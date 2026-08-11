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
