//! Audio capture for jay.
//!
//! Two independent sources feed the same pipeline: the microphone (you) and
//! system output (everyone else). They are kept on separate channels the whole
//! way through so the transcript can attribute a line to a speaker without
//! guessing from the audio itself.
//!
//! Everything downstream expects 16 kHz mono `f32`, which is what whisper.cpp
//! and Silero VAD both want, so the conversion happens here once.

use std::time::Instant;

pub mod mic;
pub mod resample;
pub mod vad;

#[cfg(target_os = "macos")]
pub mod screen;
#[cfg(target_os = "macos")]
pub mod system;

/// Sample rate every source resamples to before leaving this crate.
pub const SAMPLE_RATE: u32 = 16_000;

/// Samples per frame handed downstream. 32 ms at 16 kHz.
///
/// Not a free choice: Silero v5 accepts exactly 512 samples at 16 kHz and
/// rejects anything else, so the whole pipeline is framed to suit the VAD
/// rather than the other way round.
pub const FRAME_SAMPLES: usize = 512;

/// Frame duration implied by [`FRAME_SAMPLES`] at [`SAMPLE_RATE`].
pub const FRAME_DURATION: std::time::Duration =
    std::time::Duration::from_nanos((FRAME_SAMPLES as u64 * 1_000_000_000) / SAMPLE_RATE as u64);

/// Which side of the conversation a frame came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// The microphone. You.
    Mic,
    /// System output loopback. Whoever else is talking.
    System,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Channel::Mic => "you",
            Channel::System => "them",
        }
    }
}

/// A fixed-size window of 16 kHz mono audio, tagged with its origin.
///
/// `captured_at` is stamped in the capture callback rather than on arrival, so
/// the latency measurements later on describe the pipeline and not the queue.
#[derive(Debug, Clone)]
pub struct Frame {
    pub channel: Channel,
    pub samples: Vec<f32>,
    pub captured_at: Instant,
}

impl Frame {
    /// Root-mean-square amplitude, for level meters and for a cheap sanity
    /// check that a device is delivering something other than digital silence.
    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.samples.iter().map(|s| s * s).sum();
        (sum / self.samples.len() as f32).sqrt()
    }
}

/// Live input level, written by the capture loop and read by the panel.
///
/// This exists because of a specific failure: a session where the microphone
/// delivered nothing, and the panel showed exactly what a quiet room shows.
/// Between audio arriving and a finished sentence there are about ten seconds
/// of VAD and whisper, and for those ten seconds a dead device and a silent one
/// are indistinguishable from the outside.
///
/// So: a reading, taken from the samples themselves, updated every frame.
#[derive(Debug, Default)]
pub struct Meter {
    /// Most recent frame RMS, as `f32::to_bits`. There is no atomic float.
    rms: std::sync::atomic::AtomicU32,
    /// Frames seen. The panel watches this for *change*, not for magnitude: a
    /// level that stops moving is a stream that has died, and a frozen bar
    /// reading 0.3 would otherwise look healthier than an honest empty one.
    frames: std::sync::atomic::AtomicU64,
    /// Whether the segmenter currently has this channel open as speech.
    speaking: std::sync::atomic::AtomicBool,
    /// Heard, but not recorded.
    ///
    /// Muting in a call application mutes *that application's* stream. It does
    /// not mute the microphone, and it cannot: jay opens the device itself
    /// through CoreAudio, so a muted call is a live microphone as far as this
    /// is concerned. Everything said in the room while you are muted is
    /// transcribed and attributed to you, including the other person's voice
    /// coming back out of your own speakers.
    ///
    /// So this is a switch a person throws, because there is nothing to detect.
    /// The level keeps being recorded while it is on — a muted meter that
    /// still moves is the difference between "jay is ignoring me" and "jay has
    /// died" — and the frames stop reaching the segmenter.
    muted: std::sync::atomic::AtomicBool,
}

impl Meter {
    pub fn record(&self, rms: f32) {
        use std::sync::atomic::Ordering::Relaxed;
        self.rms.store(rms.to_bits(), Relaxed);
        self.frames.fetch_add(1, Relaxed);
    }

    pub fn set_speaking(&self, speaking: bool) {
        self.speaking
            .store(speaking, std::sync::atomic::Ordering::Relaxed);
    }

    /// Stop this channel reaching the transcript, or let it through again.
    ///
    /// Set by the panel, read by the capture loop on every frame, so a click
    /// takes effect within one 32 ms window.
    pub fn set_muted(&self, muted: bool) {
        self.muted
            .store(muted, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Current level, total frames seen, and whether the VAD is open.
    pub fn read(&self) -> (f32, u64, bool) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            f32::from_bits(self.rms.load(Relaxed)),
            self.frames.load(Relaxed),
            self.speaking.load(Relaxed),
        )
    }
}

/// One meter per channel.
#[derive(Debug, Default)]
pub struct Levels {
    pub mic: Meter,
    pub system: Meter,
}

impl Levels {
    pub fn meter(&self, channel: Channel) -> &Meter {
        match channel {
            Channel::Mic => &self.mic,
            Channel::System => &self.system,
        }
    }
}

/// RMS as a fraction of a meter's travel, on a decibel scale.
///
/// Linear RMS is useless to look at: speech sits around 0.05 to 0.3, so a
/// linear bar spends nine tenths of its length on levels no voice reaches and
/// the interesting range is a stub by the pin. -60 dBFS to 0 dBFS across the
/// full travel puts a normal speaking voice around two thirds, which is where
/// a meter should sit.
pub fn meter_fraction(rms: f32) -> f32 {
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.max(1e-6).log10();
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no {0} device available")]
    NoDevice(&'static str),
    #[error("device supports no usable input config: {0}")]
    UnsupportedConfig(String),
    #[error("resampler rejected the stream: {0}")]
    Resample(#[from] rubato::ResampleError),
    #[error("resampler could not be constructed: {0}")]
    ResamplerConstruction(#[from] rubato::ResamplerConstructionError),
    #[error("voice activity detector: {0}")]
    Vad(String),
    #[error("system audio tap: {0}")]
    SystemTap(String),
    #[error(transparent)]
    Cpal(#[from] anyhow::Error),
}

pub type Result<T> = std::result::Result<T, AudioError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_reads_as_no_travel() {
        assert_eq!(meter_fraction(0.0), 0.0);
        // Below -60 dBFS is not "very quiet", it is nothing worth drawing.
        assert_eq!(meter_fraction(0.0001), 0.0);
    }

    #[test]
    fn speech_sits_in_the_upper_half() {
        // A voice at a normal distance measures around 0.05 to 0.3 RMS. The
        // whole point of the decibel scale is that this lands somewhere a
        // person would call "most of the way", not down in the first tenth.
        let quiet = meter_fraction(0.05);
        let loud = meter_fraction(0.3);
        assert!(quiet > 0.5, "0.05 RMS drew {quiet}");
        assert!(loud > 0.8, "0.3 RMS drew {loud}");
        assert!(loud > quiet);
    }

    #[test]
    fn full_scale_does_not_overshoot_the_track() {
        assert_eq!(meter_fraction(1.0), 1.0);
        assert_eq!(meter_fraction(4.0), 1.0);
    }

    #[test]
    fn a_meter_reports_stalling_by_its_frame_count() {
        let meter = Meter::default();
        assert_eq!(meter.read(), (0.0, 0, false));
        meter.record(0.1);
        meter.record(0.2);
        let (rms, frames, speaking) = meter.read();
        assert_eq!(frames, 2, "the panel tells a dead stream from a quiet one by this");
        assert!((rms - 0.2).abs() < f32::EPSILON);
        assert!(!speaking);
        meter.set_speaking(true);
        assert!(meter.read().2);
    }
}
