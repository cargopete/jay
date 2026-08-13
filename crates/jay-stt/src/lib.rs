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
    /// The decoder's own estimate that this window contained no speech at all,
    /// taken as the worst of the segments.
    ///
    /// **Reported, not acted on.** It would be the ideal signal, and on the
    /// English-only weights jay ships with it is dead: whisper.cpp reads it off
    /// the probability of the `<|nospeech|>` token, which an `.en` vocabulary
    /// effectively never predicts. Measured 0.00 on `medium.en` for everything
    /// including eight seconds of pure digital silence, which is the strongest
    /// no-speech case that exists. Anything gating on it would be a gate that
    /// never closes, so [`judge`] does not.
    ///
    /// Kept because it costs nothing, it is worth re-checking on a multilingual
    /// model, and a zero here is now a documented zero rather than a mystery.
    pub no_speech: f32,
    /// The decoder handed back a piece of its own priming prompt.
    ///
    /// See [`is_prompt_echo`].
    pub prompt_echo: bool,
    /// Mean probability of the tokens actually emitted, in `0.0..=1.0`.
    ///
    /// Real speech decodes confidently. An invention assembled out of room
    /// tone does not, and this is the number that says so. A backend with no
    /// such signal should report `1.0`, for the same reason as above.
    pub confidence: f32,
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
    //
    // Trimmed of the subtitle dashes and stray punctuation whisper puts around
    // them first. Without that, `- (speaking in foreign language)` walked
    // straight through this check on a real session, because it does not start
    // with a bracket — it starts with the dash that introduces a speaker turn
    // in the subtitles whisper learnt from.
    let bare = normalised.trim_matches(|c: char| c == '-' || c == '.' || c.is_whitespace());
    let bracketed = (bare.starts_with('[') && bare.ends_with(']'))
        || (bare.starts_with('(') && bare.ends_with(')'));
    if bracketed {
        return true;
    }

    OUTRO_FRAGMENTS
        .iter()
        .any(|fragment| normalised.contains(fragment))
}

/// Is this transcript a piece of the prompt the decoder was primed with?
///
/// Priming whisper with the vocabulary of the round is the cheapest accuracy
/// available, and it buys a great deal — it is the difference between "reverse
/// a singly linked list" and "reverse the link please". It also creates a new
/// way to invent text, which cost a real session:
///
/// ```text
/// [00:12] them: Redis  Kafka
/// [00:20] them: Redis  Kafka
/// ```
///
/// Nobody said that. The session was primed with `--vocab "… Redis, Kafka"`,
/// the system channel had nothing on it, and the decoder reached for its own
/// prompt. Those clear every other filter: they are not known artefacts, they
/// are not quiet, and they are decoded confidently, because the model is
/// copying rather than guessing.
///
/// The test is whether every word of the transcript appears in the prompt, *in
/// the prompt's own order*. Contiguity is too strict: the first attempt at this
/// required an unbroken run and was immediately beaten by
///
/// ```text
/// [00:04] you: Postgres  nginx  Kafka
/// ```
///
/// which skips "Redis" from the middle of the list it was reciting. Order is
/// the property that matters, because a vocabulary list's order is arbitrary.
/// Real speech that uses those words puts its own words between them — "we
/// would put Redis in front of it" fails on "we" alone — and real speech that
/// happens to name two of them usually names them in its own order.
///
/// Two words minimum, because a lone "Postgres" is plausibly something somebody
/// said. The known cost of this rule: someone who genuinely says nothing but
/// "Postgres, Redis", in that order, loses it. That is two words of an
/// utterance nobody could act on, against fabricated lines that get spent as
/// context, and the trade is worth it.
pub fn is_prompt_echo(text: &str, prompt: &str) -> bool {
    let spoken = words(text);
    if spoken.len() < 2 {
        return false;
    }
    let primed = words(prompt);

    // Greedy in-order match, restarted from each place the first word occurs.
    // Greedy from a single start is not enough: the prompt repeats terms, and
    // the run that matches may begin at the second occurrence.
    primed
        .iter()
        .enumerate()
        .filter(|(_, word)| *word == &spoken[0])
        .any(|(start, _)| {
            let mut wanted = spoken.iter().skip(1);
            let mut next = wanted.next();
            for word in &primed[start + 1..] {
                match next {
                    None => break,
                    Some(target) if word == target => next = wanted.next(),
                    Some(_) => {}
                }
            }
            next.is_none()
        })
}

/// Lowercased words, stripped of the punctuation that separates a vocabulary
/// list from a sentence.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_ascii_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Speech peak below which a transcript is treated as invented.
///
/// Compared against the loudest frame the VAD called speech, never against the
/// mean over the whole utterance. The mean includes ~250 ms of pre-roll and
/// ~600 ms of trailing silence, so it scales with how long the pause afterwards
/// was rather than with how loudly anyone spoke — which quietly binned a
/// perfectly clear "reverse a linked list" on a machine whose room floor
/// measures 0.0028.
///
/// A single frame peak from a voice at a laptop's distance clears this by a
/// wide margin. Room tone does not.
pub const SPEECH_PEAK_FLOOR: f32 = 0.01;

/// Reject when the mean token probability falls below this.
///
/// Deliberately low. The evidence behind it is thin: `say`-synthesised speech
/// decodes at 0.88, digital silence captioned "Thank you for watching." at
/// 0.48, and noise whisper correctly labels `[static]` sits anywhere from 0.68
/// to 0.98. That last range is the problem — confidence alone does not separate
/// noise from speech, it only catches the decoder writing something it plainly
/// does not believe.
///
/// So this is a backstop behind the artefact list and the level floor, set to
/// err toward keeping a real quiet sentence rather than binning one. Raising it
/// requires a corpus of genuine fluent inventions, which needs live sessions to
/// collect. See `tests/silence.rs`.
pub const CONFIDENCE_FLOOR: f32 = 0.45;

/// Why a transcript was not allowed into the conversation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rejected {
    /// A phrase whisper is known to produce from nothing.
    KnownArtefact,
    /// The audio was never loud enough to have been speech.
    TooQuiet { peak: f32 },
    /// The decoder wrote it without believing it.
    Invented { confidence: f32 },
    /// The decoder handed back part of its own priming vocabulary.
    PrimingEcho,
}

impl Rejected {
    /// What the panel says. Said out loud rather than logged, because a session
    /// where everything is binned must not look like a session where nobody
    /// spoke.
    pub fn notice(&self, spoken: Duration) -> String {
        match self {
            Rejected::KnownArtefact => "dropped a known whisper artefact".to_string(),
            Rejected::TooQuiet { peak } => format!(
                "heard {:.1}s at peak {peak:.4} — too quiet to trust, dropped",
                spoken.as_secs_f32()
            ),
            Rejected::Invented { confidence } => format!(
                "dropped {:.1}s the transcriber did not believe (confidence {confidence:.2})",
                spoken.as_secs_f32()
            ),
            Rejected::PrimingEcho => {
                "dropped the transcriber reciting its own --vocab back".to_string()
            }
        }
    }
}

/// Decide whether one transcript is worth keeping.
///
/// Every reason a transcript gets binned lives here, because they used to live
/// in two separate `continue`s in the capture loop and a third was needed.
/// `speech_peak` is the loudest frame the VAD called speech; a caller with no
/// VAD should pass [`f32::INFINITY`] rather than zero, since the check is a
/// lower bound and zero would reject everything.
pub fn judge(transcription: &Transcription, speech_peak: f32) -> Option<Rejected> {
    if is_hallucination(&transcription.text) {
        return Some(Rejected::KnownArtefact);
    }
    if transcription.prompt_echo {
        return Some(Rejected::PrimingEcho);
    }
    if speech_peak < SPEECH_PEAK_FLOOR {
        return Some(Rejected::TooQuiet { peak: speech_peak });
    }
    if transcription.confidence < CONFIDENCE_FLOOR {
        return Some(Rejected::Invented {
            confidence: transcription.confidence,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spoken(text: &str, no_speech: f32, confidence: f32) -> Transcription {
        Transcription {
            text: text.to_string(),
            inference: Duration::from_millis(1),
            no_speech,
            confidence,
            prompt_echo: false,
        }
    }

    /// The vocabulary from the session that produced the bug.
    const PRIMED: &str = "Likely terms: linked list, binary tree, hash map, \
        big O, Postgres, Kafka, Redis, S3. Also: pastebin, base62, flood fill, \
        union-find, usize, HAProxy, nginx, MinIO, Patroni, Redis, Kafka";

    #[test]
    fn the_transcriber_reciting_its_own_vocabulary_is_caught() {
        // Verbatim from the archive of a real session, twice on a silent
        // channel. The double space is how it arrived.
        assert!(is_prompt_echo("Redis  Kafka", PRIMED));
        assert!(is_prompt_echo("MinIO, Patroni", PRIMED));
        assert!(is_prompt_echo("hash map, big O", PRIMED));
    }

    /// Also verbatim, and the reason contiguity was not enough: this one skips
    /// "Redis" out of the middle of the list it is reciting.
    #[test]
    fn a_recitation_that_skips_a_term_is_still_a_recitation() {
        const SHORT: &str = "pastebin, base62, Postgres, nginx, Redis, Kafka";
        assert!(is_prompt_echo("Postgres  nginx  Kafka", SHORT));
        assert!(is_prompt_echo("pastebin, Kafka", SHORT));
    }

    /// Order is the whole discriminator, so backwards must not match.
    #[test]
    fn the_same_terms_in_another_order_are_not_a_recitation() {
        const SHORT: &str = "pastebin, base62, Postgres, nginx";
        assert!(!is_prompt_echo("nginx Postgres", SHORT));
    }

    #[test]
    fn real_speech_using_those_words_is_left_alone() {
        // The words are primed; the sentence is not the prompt.
        assert!(!is_prompt_echo("we would put Redis in front of it", PRIMED));
        assert!(!is_prompt_echo("Kafka and Redis", PRIMED));
        assert!(!is_prompt_echo("so, Postgres", PRIMED));
        // One word is never enough. Somebody may simply have said it.
        assert!(!is_prompt_echo("Postgres", PRIMED));
        assert!(!is_prompt_echo("nginx", PRIMED));
    }

    #[test]
    fn a_primed_echo_is_binned_even_though_it_reads_confidently() {
        let mut echo = spoken("Redis Kafka", 0.0, 0.97);
        echo.prompt_echo = true;
        assert_eq!(judge(&echo, 0.4), Some(Rejected::PrimingEcho));
    }

    #[test]
    fn judge_keeps_confident_speech_and_bins_the_rest() {
        let real = spoken("reverse a singly linked list in place", 0.02, 0.93);
        assert_eq!(judge(&real, 0.4), None);

        assert_eq!(
            judge(&spoken("[BLANK_AUDIO]", 0.0, 1.0), 0.4),
            Some(Rejected::KnownArtefact)
        );
        assert_eq!(
            judge(&real, 0.001),
            Some(Rejected::TooQuiet { peak: 0.001 })
        );
        // Loud enough, not a known phrase, and written by a decoder that did
        // not believe a word of it.
        assert!(matches!(
            judge(&spoken("Cool? Distinct.", 0.91, 0.34), 0.4),
            Some(Rejected::Invented { .. })
        ));
    }

    /// The signal is dead on `.en` weights, so nothing may depend on it. If a
    /// future model makes it live, this test is where to notice.
    #[test]
    fn no_speech_is_reported_but_never_decides() {
        let certain_silence = spoken("a sentence from nowhere", 1.0, 0.99);
        assert_eq!(judge(&certain_silence, 0.4), None);
    }

    /// The artefact check runs first so its notice is the specific one, and a
    /// quiet artefact does not get reported as merely quiet.
    #[test]
    fn reasons_are_reported_most_specific_first() {
        assert_eq!(
            judge(&spoken("Thank you.", 0.99, 0.1), 0.0001),
            Some(Rejected::KnownArtefact)
        );
    }

    /// A backend without confidence signals reports 0.0/1.0, which must read as
    /// "no opinion" rather than as a reason to bin everything it produces.
    #[test]
    fn a_backend_with_no_confidence_signal_is_not_penalised() {
        assert_eq!(judge(&spoken("a real sentence here", 0.0, 1.0), 0.4), None);
    }

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
            // The subtitle dash, which walked through the bracket check on a
            // real session because the line does not begin with a bracket.
            "- (speaking in foreign language)",
            "-[BLANK_AUDIO]",
            "- (music) .",
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
