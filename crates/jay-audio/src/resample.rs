//! Rate conversion from whatever a device hands us down to 16 kHz mono.
//!
//! Devices rarely offer 16 kHz directly. The mic on this machine runs at 48 kHz
//! and system loopback usually matches whatever the output device is doing, so
//! everything gets funnelled through here before the VAD or whisper see it.
//!
//! Naive decimation would be cheaper and would also alias every consonant into
//! mush, which shows up later as a worse word error rate and is very hard to
//! diagnose from the transcript alone. So: a proper polyphase resampler.

use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Async, FixedAsync, PolynomialDegree, Resampler};

use crate::{FRAME_SAMPLES, Result, SAMPLE_RATE};

/// Input frames consumed per resampler call. Large enough that the polynomial
/// fit is not dominated by call overhead, small enough that a 48 kHz stream
/// still turns over roughly every 21 ms.
const CHUNK_IN: usize = 1024;

/// Converts a mono stream at some arbitrary device rate into 16 kHz frames.
///
/// Feed it with [`push`](Self::push) and drain it with
/// [`take_frame`](Self::take_frame); the two are decoupled because a device
/// callback delivers whatever buffer size it feels like and we want fixed
/// frames coming out the other end.
pub struct Downsampler {
    /// `None` when the device is already at 16 kHz and the samples pass straight through.
    inner: Option<Async<f32>>,
    pending: Vec<f32>,
    ready: Vec<f32>,
    scratch: Vec<f32>,
}

impl Downsampler {
    pub fn new(input_rate: u32) -> Result<Self> {
        if input_rate == SAMPLE_RATE {
            return Ok(Self {
                inner: None,
                pending: Vec::new(),
                ready: Vec::new(),
                scratch: Vec::new(),
            });
        }

        let ratio = f64::from(SAMPLE_RATE) / f64::from(input_rate);
        let resampler = Async::<f32>::new_poly(
            ratio,
            // The ratio is fixed, so the resampler needs no headroom to move.
            1.1,
            // Septic is the expensive end of the polynomial family and still
            // far cheaper than sinc. Speech at 16 kHz does not need better.
            PolynomialDegree::Septic,
            CHUNK_IN,
            1,
            FixedAsync::Input,
        )?;

        let scratch = vec![0.0; resampler.output_frames_max()];
        Ok(Self {
            inner: Some(resampler),
            pending: Vec::with_capacity(CHUNK_IN * 2),
            ready: Vec::with_capacity(FRAME_SAMPLES * 8),
            scratch,
        })
    }

    /// Absorb mono samples at the device rate.
    pub fn push(&mut self, mono: &[f32]) -> Result<()> {
        let Some(resampler) = self.inner.as_mut() else {
            self.ready.extend_from_slice(mono);
            return Ok(());
        };

        self.pending.extend_from_slice(mono);

        let mut consumed = 0;
        while self.pending.len() - consumed >= CHUNK_IN {
            let input = InterleavedSlice::new(&self.pending[consumed..consumed + CHUNK_IN], 1, CHUNK_IN)
                .expect("chunk length matches the declared frame count");
            let capacity = self.scratch.len();
            let mut output = InterleavedSlice::new_mut(&mut self.scratch, 1, capacity)
                .expect("scratch length matches the declared frame count");

            let (used, produced) = resampler.process_into_buffer(&input, &mut output, None)?;
            self.ready.extend_from_slice(&self.scratch[..produced]);
            consumed += used;
        }

        self.pending.drain(..consumed);
        Ok(())
    }

    /// Pop one fixed-size frame, if a whole one is available.
    pub fn take_frame(&mut self) -> Option<Vec<f32>> {
        if self.ready.len() < FRAME_SAMPLES {
            return None;
        }
        let frame: Vec<f32> = self.ready.drain(..FRAME_SAMPLES).collect();
        Some(frame)
    }

    /// Samples resampled but not yet drained into a frame.
    pub fn buffered(&self) -> usize {
        self.ready.len()
    }
}

/// Fold an interleaved multi-channel buffer down to mono by averaging.
///
/// Loopback capture is usually stereo and speech is usually centred, so the
/// average keeps the voice and quietly cancels anything hard-panned. Taking
/// only the left channel instead would drop a speaker who happens to be
/// panned right, which is the sort of thing you discover much too late.
pub fn downmix(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    out.clear();
    if channels <= 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    let scale = 1.0 / channels as f32;
    out.reserve(interleaved.len() / channels);
    for frame in interleaved.chunks_exact(channels) {
        out.push(frame.iter().sum::<f32>() * scale);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passthrough_when_already_at_target_rate() {
        let mut d = Downsampler::new(SAMPLE_RATE).unwrap();
        d.push(&vec![0.5; FRAME_SAMPLES * 2]).unwrap();
        assert_eq!(d.take_frame().unwrap().len(), FRAME_SAMPLES);
        assert!(d.take_frame().is_some());
        assert!(d.take_frame().is_none());
    }

    #[test]
    fn downsamples_48k_to_roughly_a_third() {
        let mut d = Downsampler::new(48_000).unwrap();
        // One second of 48 kHz input should yield about one second at 16 kHz.
        let input = vec![0.0; 48_000];
        d.push(&input).unwrap();

        let mut frames = 0;
        while d.take_frame().is_some() {
            frames += 1;
        }
        let produced = frames * FRAME_SAMPLES + d.buffered();
        // Allow for the resampler's internal delay holding back a little.
        assert!(
            (15_000..=16_000).contains(&produced),
            "expected ~16000 samples out, got {produced}"
        );
    }

    #[test]
    fn downmix_averages_stereo() {
        let mut out = Vec::new();
        downmix(&[1.0, 0.0, 0.5, 0.5], 2, &mut out);
        assert_eq!(out, vec![0.5, 0.5]);
    }

    #[test]
    fn downmix_passes_mono_through() {
        let mut out = Vec::new();
        downmix(&[0.1, 0.2, 0.3], 1, &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3]);
    }
}
