//! Speech segmentation with Silero VAD.
//!
//! Whisper is expensive and produces confident nonsense when handed silence,
//! so nothing reaches it that the VAD has not first agreed is speech. This
//! module turns a stream of fixed frames into whole utterances with their
//! boundaries.
//!
//! The Silero weights are compiled into the binary by the
//! `voice_activity_detector` crate, so there is no model to download and no
//! first-run network call.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use voice_activity_detector::VoiceActivityDetector;

use crate::{AudioError, Channel, FRAME_DURATION, FRAME_SAMPLES, Frame, Result, SAMPLE_RATE};

/// Probability above which a frame counts as speech.
///
/// Silero's own examples use 0.5. Lower catches quiet talkers along with more
/// keyboard noise; higher clips the start of words.
const SPEECH_THRESHOLD: f32 = 0.5;

/// Consecutive speech frames needed to open an utterance. Two frames is 64 ms,
/// enough to ignore a single chair creak without swallowing a short word.
const ENTRY_FRAMES: usize = 2;

/// Consecutive silent frames that close an utterance. About 600 ms, which is
/// long enough to survive the pause in the middle of a sentence and short
/// enough that the transcript does not lag a whole thought behind.
const EXIT_FRAMES: usize = 19;

/// Frames retained before speech is confirmed, so the onset is not clipped.
/// About 250 ms, which comfortably covers the plosive at the start of a word.
const PRE_ROLL_FRAMES: usize = 8;

/// Hard cap on a single utterance. Whisper's context window is 30 seconds and
/// a monologue longer than this should be cut rather than silently truncated
/// somewhere deeper in the pipeline.
const MAX_UTTERANCE: Duration = Duration::from_secs(25);

/// A contiguous stretch of speech from one channel, ready to transcribe.
#[derive(Debug, Clone)]
pub struct Utterance {
    pub channel: Channel,
    pub samples: Vec<f32>,
    /// When the first frame of this utterance was captured.
    pub started_at: Instant,
    /// Loudest frame the VAD actually called speech.
    ///
    /// This is the honest measure of "was anything really said", and [`rms`]
    /// is not. Every utterance carries about 250 ms of pre-roll and 600 ms of
    /// trailing silence by construction, so a mean over the whole buffer is a
    /// mean over a good deal of room tone: a perfectly clear short sentence
    /// can be diluted below a threshold a long one clears easily. A caller
    /// that discards quiet audio must judge it on this.
    pub speech_peak: f32,
}

impl Utterance {
    /// Root-mean-square amplitude over the whole utterance, silence included.
    ///
    /// Useful as a level reading. Not useful as a threshold — see
    /// [`speech_peak`](Self::speech_peak), which is what to gate on.
    pub fn rms(&self) -> f32 {
        if self.samples.is_empty() {
            return 0.0;
        }
        let sum: f32 = self.samples.iter().map(|s| s * s).sum();
        (sum / self.samples.len() as f32).sqrt()
    }

    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(SAMPLE_RATE))
    }
}

/// Turns frames into utterances for a single channel.
///
/// One segmenter per channel: the VAD carries recurrent state across calls, so
/// interleaving two speakers through one instance would corrupt both.
pub struct SpeechSegmenter {
    vad: VoiceActivityDetector,
    channel: Channel,
    in_speech: bool,
    speech_run: usize,
    silence_run: usize,
    current: Vec<f32>,
    pre_roll: VecDeque<Vec<f32>>,
    started_at: Option<Instant>,
    /// Loudest frame the VAD has called speech in the utterance being built.
    speech_peak: f32,
}

impl SpeechSegmenter {
    pub fn new(channel: Channel) -> Result<Self> {
        let vad = VoiceActivityDetector::builder()
            .sample_rate(SAMPLE_RATE as i64)
            .chunk_size(FRAME_SAMPLES)
            .build()
            .map_err(|e| AudioError::Vad(e.to_string()))?;

        Ok(Self {
            vad,
            channel,
            in_speech: false,
            speech_run: 0,
            silence_run: 0,
            current: Vec::new(),
            pre_roll: VecDeque::with_capacity(PRE_ROLL_FRAMES),
            started_at: None,
            speech_peak: 0.0,
        })
    }

    /// Feed one frame. Returns an utterance when one has just ended.
    pub fn push(&mut self, frame: &Frame) -> Option<Utterance> {
        debug_assert_eq!(frame.channel, self.channel);
        debug_assert_eq!(frame.samples.len(), FRAME_SAMPLES);

        let probability = self.vad.predict(frame.samples.iter().copied());
        let is_speech = probability >= SPEECH_THRESHOLD;

        // Track the peak over speech frames only, so the level survives the
        // pre-roll and trailing silence that bracket every utterance.
        if is_speech {
            self.speech_peak = self.speech_peak.max(frame.rms());
        }

        if self.in_speech {
            self.current.extend_from_slice(&frame.samples);
            if is_speech {
                self.silence_run = 0;
            } else {
                self.silence_run += 1;
            }

            let too_long = self.duration_of_current() >= MAX_UTTERANCE;
            if self.silence_run >= EXIT_FRAMES || too_long {
                if too_long {
                    tracing::debug!(
                        channel = self.channel.label(),
                        "utterance hit the length cap, cutting it here"
                    );
                }
                return self.close();
            }
            None
        } else {
            if is_speech {
                self.speech_run += 1;
            } else {
                self.speech_run = 0;
                // A stray speech frame that never became an utterance must not
                // leave its level behind to flatter the next one.
                self.speech_peak = 0.0;
            }

            self.pre_roll.push_back(frame.samples.clone());
            if self.pre_roll.len() > PRE_ROLL_FRAMES {
                self.pre_roll.pop_front();
            }

            if self.speech_run >= ENTRY_FRAMES {
                self.open(frame.captured_at);
            }
            None
        }
    }

    /// Close any utterance in progress, for shutdown or a mode change.
    pub fn flush(&mut self) -> Option<Utterance> {
        if self.in_speech { self.close() } else { None }
    }

    pub fn is_speaking(&self) -> bool {
        self.in_speech
    }

    fn open(&mut self, frame_time: Instant) {
        self.in_speech = true;
        self.silence_run = 0;
        self.current.clear();

        // The pre-roll already holds this frame, so the utterance starts
        // however far back the retained frames reach.
        let retained = self.pre_roll.len();
        for buffered in self.pre_roll.drain(..) {
            self.current.extend_from_slice(&buffered);
        }
        self.started_at = Some(frame_time - FRAME_DURATION * (retained.saturating_sub(1)) as u32);
    }

    fn close(&mut self) -> Option<Utterance> {
        self.in_speech = false;
        self.speech_run = 0;
        self.silence_run = 0;
        self.pre_roll.clear();

        let samples = std::mem::take(&mut self.current);
        let speech_peak = std::mem::take(&mut self.speech_peak);
        let started_at = self.started_at.take()?;
        if samples.is_empty() {
            return None;
        }

        Some(Utterance {
            channel: self.channel,
            samples,
            started_at,
            speech_peak,
        })
    }

    fn duration_of_current(&self) -> Duration {
        Duration::from_secs_f64(self.current.len() as f64 / f64::from(SAMPLE_RATE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(samples: Vec<f32>) -> Frame {
        Frame {
            channel: Channel::Mic,
            samples,
            captured_at: Instant::now(),
        }
    }

    #[test]
    fn silence_never_opens_an_utterance() {
        let mut seg = SpeechSegmenter::new(Channel::Mic).unwrap();
        for _ in 0..100 {
            assert!(seg.push(&frame(vec![0.0; FRAME_SAMPLES])).is_none());
        }
        assert!(!seg.is_speaking());
        assert!(seg.flush().is_none());
    }

    #[test]
    fn white_noise_is_not_mistaken_for_speech() {
        // A cheap deterministic noise source. Silero should decline to call
        // this speech; if it does not, the threshold needs revisiting.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed as u32 as f32 / u32::MAX as f32) * 0.4 - 0.2
        };

        let mut seg = SpeechSegmenter::new(Channel::Mic).unwrap();
        let mut utterances = 0;
        for _ in 0..80 {
            let samples: Vec<f32> = (0..FRAME_SAMPLES).map(|_| noise()).collect();
            if seg.push(&frame(samples)).is_some() {
                utterances += 1;
            }
        }
        assert_eq!(utterances, 0, "white noise was segmented as speech");
    }
}
