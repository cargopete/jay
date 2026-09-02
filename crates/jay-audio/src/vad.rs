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

/// Consecutive silent frames that close an utterance. About one second.
///
/// This was 600 ms, chosen to keep the transcript close behind the speaker.
/// Watching a real person think aloud showed what that costs: "Hello, testing,
/// um, do you-" / "We think we can..." / "Um... reverse." / "A linked list."
/// — one thought, four utterances, each transcribed without the context of the
/// others, and whisper is markedly worse on a two-word fragment than on a
/// sentence.
///
/// A longer window used to mean the transcript lagged at the moment it mattered
/// most, which was a fair objection. It no longer holds: pressing the lever now
/// flushes whatever is mid-sentence and waits for it, so the wait is paid only
/// when somebody is actually reading, and never while they are still talking.
const EXIT_FRAMES: usize = 31;

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
        self.push_gated(frame, false)
    }

    /// Feed one frame, optionally holding this channel shut.
    ///
    /// `gated` says the *other* channel is mid-utterance, which on a call taken
    /// without headphones means everything arriving here is the far side coming
    /// back off the speakers. Left ungated, that is not merely a duplicate
    /// line: the microphone never falls silent, so [`EXIT_FRAMES`] is never
    /// reached, and utterances run to [`MAX_UTTERANCE`] and are cut at 25
    /// seconds with both people's words inside them. Nineteen of fifty-five, in
    /// one real meeting.
    ///
    /// A gated frame is still fed to the VAD, because the detector is recurrent
    /// and skipping frames would corrupt the state it carries. It simply cannot
    /// open an utterance, and counts as silence towards closing one already
    /// open — which is what lets a sentence interrupted by the far side end
    /// cleanly rather than absorbing them.
    ///
    /// The cost is honest and one-sided: speak while the other person is
    /// speaking and you are not transcribed. That is worse than a perfect
    /// echo canceller and better than what it replaces, where the same words
    /// survive inside a 25-second block attributed to the wrong person.
    pub fn push_gated(&mut self, frame: &Frame, gated: bool) -> Option<Utterance> {
        debug_assert_eq!(frame.channel, self.channel);
        debug_assert_eq!(frame.samples.len(), FRAME_SAMPLES);

        let probability = self.vad.predict(frame.samples.iter().copied());
        let is_speech = probability >= SPEECH_THRESHOLD && !gated;

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

    /// Mid-utterance, *or* one speech frame into opening one.
    ///
    /// What the echo gate must ask, rather than [`is_speaking`](Self::is_speaking).
    /// Both channels hear the far side's first syllable in the same 32 ms frame
    /// and both need [`ENTRY_FRAMES`] before they will open, so which one opens
    /// first is decided by the order the two frames happen to come off the
    /// channel. When the microphone wins that race it opens on the echo with
    /// the gate still down, and then holds the fragment for a full
    /// [`EXIT_FRAMES`] hangover before letting go.
    ///
    /// That is not theoretical. A 44-minute call produced 52 of them, one at the
    /// start of nearly every turn, each one or two seconds long and made of the
    /// opening words of the `them` line directly beneath it. They cannot be
    /// caught downstream: the microphone's copy of half a word transcribes to
    /// something that looks nothing like the system's copy of the whole
    /// sentence, so there is no text for the echo guard to match.
    ///
    /// One frame of margin is enough, and it costs a stray noise frame gating
    /// the other channel for 32 ms.
    pub fn is_or_becoming_speech(&self) -> bool {
        self.in_speech || self.speech_run > 0
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

    /// 3.36 seconds of real speech at 16 kHz mono, signed 16-bit little-endian
    /// and headerless. Synthesised with `say` rather than recorded, so it can
    /// live in the repository without anybody's voice in it, and it is the only
    /// input here that Silero actually calls speech. Every test below that
    /// claims something about an utterance needs one.
    const SPEECH: &[u8] = include_bytes!("../testdata/speech-16k-mono.pcm");

    fn speech_frames() -> Vec<Vec<f32>> {
        let samples: Vec<f32> = SPEECH
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&pair| i16::from_le_bytes(pair) as f32 / 32768.0)
            .collect();
        samples
            .as_chunks::<FRAME_SAMPLES>()
            .0
            .iter()
            .map(|chunk| chunk.to_vec())
            .collect()
    }

    /// The fixture is only useful if the detector agrees it is speech. Asserted
    /// on its own so that a failure here reads as "the fixture or the detector
    /// moved" rather than being blamed on whatever the gate did.
    #[test]
    fn the_speech_fixture_is_heard_as_speech() {
        let mut seg = SpeechSegmenter::new(Channel::Mic).unwrap();
        let mut utterances = 0;
        for samples in speech_frames() {
            if seg.push(&frame(samples)).is_some() {
                utterances += 1;
            }
        }
        utterances += usize::from(seg.flush().is_some());
        assert!(utterances > 0, "the speech fixture never opened an utterance");
    }

    /// The echo gate. Without headphones the far side arrives at the microphone
    /// louder than speech, and an ungated segmenter never falls silent: it runs
    /// to `MAX_UTTERANCE` and cuts at 25 seconds with both people inside.
    #[test]
    fn a_gated_channel_never_opens_an_utterance() {
        let mut seg = SpeechSegmenter::new(Channel::Mic).unwrap();
        for samples in speech_frames() {
            assert!(
                seg.push_gated(&frame(samples), true).is_none(),
                "the gate let an utterance through"
            );
            assert!(!seg.is_speaking(), "the gate let the segmenter open");
        }
        assert!(seg.flush().is_none(), "the gate left an utterance behind");
    }

    /// Closing matters as much as not opening. A sentence already in progress
    /// when the other person starts talking has to end, rather than absorbing
    /// them and running on to the cap.
    #[test]
    fn gating_closes_an_utterance_already_open() {
        let frames = speech_frames();
        let mut seg = SpeechSegmenter::new(Channel::Mic).unwrap();

        // Open one for real on the first half of the fixture.
        let half = frames.len() / 2;
        for samples in frames.iter().take(half) {
            seg.push(&frame(samples.clone()));
        }
        assert!(seg.is_speaking(), "the fixture did not open an utterance");

        // Now the far side starts. The gate counts as silence, so the utterance
        // closes on the normal hangover rather than running on.
        let mut closed = false;
        for samples in frames.iter().skip(half) {
            if seg.push_gated(&frame(samples.clone()), true).is_some() {
                closed = true;
                break;
            }
        }
        assert!(closed, "gating never closed the open utterance");
        assert!(!seg.is_speaking());
    }

    /// The gate has to fire a frame before the segmenter opens, or the other
    /// channel wins the race and opens on the echo. This is the margin.
    #[test]
    fn becoming_speech_is_true_before_the_utterance_opens() {
        let mut seg = SpeechSegmenter::new(Channel::Mic).unwrap();
        assert!(!seg.is_or_becoming_speech(), "idle channel claimed speech");

        let mut saw_the_gap = false;
        for samples in speech_frames() {
            seg.push(&frame(samples));
            if seg.is_or_becoming_speech() && !seg.is_speaking() {
                saw_the_gap = true;
            }
            if seg.is_speaking() {
                break;
            }
        }
        assert!(
            saw_the_gap,
            "opened without ever reporting that it was about to"
        );
        assert!(seg.is_speaking(), "the fixture never opened an utterance");
        assert!(seg.is_or_becoming_speech(), "open but not reported as such");
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
