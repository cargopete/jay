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
    /// Exact matches: bracketed markers and bare artefacts.
    const ARTEFACTS: &[&str] = &[
        "[blank_audio]",
        "(blank_audio)",
        "[silence]",
        "[music]",
        "(upbeat music)",
        "thank you.",
        "you",
        ".",
    ];

    /// Phrases whisper produces from silence, in whole or in part.
    ///
    /// These are not guesses. Whisper's training data is heavy with subtitled
    /// video, so given nothing to transcribe it reaches for the way videos
    /// end. A 90-second recording of an empty room produced "I'll see you next
    /// time" unprompted, which in an interview would quietly poison the
    /// context with a sentence nobody said.
    const OUTRO_FRAGMENTS: &[&str] = &[
        "thanks for watching",
        "thank you for watching",
        "see you next time",
        "see you in the next",
        "don't forget to subscribe",
        "like and subscribe",
        "subscribe to my channel",
        "hit the bell",
        "please subscribe",
        "bye bye",
        "the end",
        "transcription by",
        "subtitles by",
        "amara.org",
        "www.",
        ".com",
    ];

    let normalised = text.trim().to_ascii_lowercase();
    if normalised.is_empty() || ARTEFACTS.contains(&normalised.as_str()) {
        return true;
    }

    // Bracketed sound markers of any kind: [BLANK_AUDIO], (typing), [laughs].
    let bracketed = (normalised.starts_with('[') && normalised.ends_with(']'))
        || (normalised.starts_with('(') && normalised.ends_with(')'));
    if bracketed {
        return true;
    }

    OUTRO_FRAGMENTS
        .iter()
        .any(|fragment| normalised.contains(fragment))
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

    /// Real output from a 90-second recording of an empty room.
    #[test]
    fn catches_the_youtube_outros_whisper_invents_from_silence() {
        for line in [
            "I'll see you next time.",
            "Thanks for watching!",
            "Thank you for watching.",
            "Don't forget to subscribe!",
            "Bye bye.",
            "[typing]",
            "(door closes)",
        ] {
            assert!(is_hallucination(line), "should have been dropped: {line}");
        }
    }

    #[test]
    fn leaves_real_speech_alone() {
        assert!(!is_hallucination("thank you for the detailed explanation"));
        assert!(!is_hallucination("why is this test failing"));
        // "you" alone is an artefact; "you" in a sentence plainly is not.
        assert!(!is_hallucination("you were right about the lock"));
        // "see you next time" is an artefact; this is a real sentence that
        // happens to be about subscribing.
        assert!(!is_hallucination(
            "the subscriber sees events in the order the log wrote them"
        ));
    }
}
