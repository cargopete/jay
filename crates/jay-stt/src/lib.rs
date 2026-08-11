//! Speech to text for jay.
//!
//! One trait, several possible backends. The first is whisper.cpp on Metal,
//! which is the pragmatic local choice on Apple silicon. Parakeet through
//! CoreML and a cloud backend for people who would rather pay than wait both
//! fit the same shape, which is the reason for the trait.

use std::time::Duration;

pub mod models;
pub mod whisper;

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("model file not found at {0}")]
    ModelMissing(std::path::PathBuf),
    #[error("downloading model: {0}")]
    Download(String),
    #[error("whisper: {0}")]
    Whisper(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SttError>;

/// What a backend gives back for one utterance.
#[derive(Debug, Clone)]
pub struct Transcription {
    pub text: String,
    /// How long inference took, which is the half of the latency budget that
    /// is actually ours to control.
    pub inference: Duration,
}

/// A local or remote speech recogniser.
///
/// `&mut self` rather than `&self` because whisper carries decoder state and
/// the borrow makes it obvious that one instance transcribes one thing at a
/// time. Run two if you want two channels in parallel.
pub trait SpeechModel: Send {
    /// Transcribe one complete utterance of 16 kHz mono `f32`.
    fn transcribe(&mut self, samples: &[f32]) -> Result<Transcription>;

    /// Human-readable identifier, for logs and for the overlay's status line.
    fn name(&self) -> &str;
}

/// Strip the artefacts whisper emits when handed near-silence.
///
/// Even gated by a VAD, a short breath or a door closing occasionally reaches
/// the model, and whisper is famously willing to fill the gap with "Thank
/// you." or "[BLANK_AUDIO]" delivered with total confidence. These are worth
/// dropping before they reach a transcript that something else will reason
/// over.
pub fn is_hallucination(text: &str) -> bool {
    const ARTEFACTS: &[&str] = &[
        "[blank_audio]",
        "(blank_audio)",
        "[silence]",
        "[music]",
        "(upbeat music)",
        "thank you.",
        "thanks for watching!",
        "you",
        ".",
    ];

    let normalised = text.trim().to_ascii_lowercase();
    normalised.is_empty() || ARTEFACTS.contains(&normalised.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_usual_whisper_artefacts() {
        assert!(is_hallucination(""));
        assert!(is_hallucination("   "));
        assert!(is_hallucination("[BLANK_AUDIO]"));
        assert!(is_hallucination("Thank you."));
        assert!(is_hallucination("you"));
    }

    #[test]
    fn leaves_real_speech_alone() {
        assert!(!is_hallucination("thank you for the detailed explanation"));
        assert!(!is_hallucination("why is this test failing"));
        // "you" alone is an artefact; "you" in a sentence plainly is not.
        assert!(!is_hallucination("you were right about the lock"));
    }
}
