//! The gate: deciding when jay should say something.
//!
//! This runs on every utterance, so it must be free. It is deliberately not a
//! model. Routing each utterance through `claude -p` was measured at $0.0254
//! cold and $0.0033 with a warm cache, because the CLI ships roughly 29,000
//! tokens of Claude Code preamble on every invocation regardless of how small
//! the question is. At one call every thirty seconds that is about $0.40 an
//! hour warm, for a classifier answering yes or no.
//!
//! So the gate is rules, and the subscription pays only for the escalation.

/// Why jay decided to speak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trigger {
    /// Someone asked a question. The payload is the question as heard.
    Question(String),
    /// The user asked jay directly, by wake phrase or hotkey.
    Addressed(String),
    /// A deterministic event: a test went red, a stack trace appeared.
    Event(String),
}

/// Words that open a question often enough to be worth acting on, without
/// the question mark that whisper does not always supply.
const INTERROGATIVES: &[&str] = &[
    "what", "why", "how", "when", "where", "who", "which", "whose", "can you",
    "could you", "would you", "will you", "do you", "did you", "have you",
    "are you", "is there", "tell me", "walk me", "describe", "explain",
    "suppose", "imagine", "given",
];

/// Phrases that address jay directly.
const WAKE_PHRASES: &[&str] = &["hey jay", "ok jay", "okay jay", "jay,"];

/// Minimum words before a question is worth escalating.
///
/// "What?" is a request for repetition, not a question worth an answer, and
/// escalating on it would be both expensive and irritating.
const MIN_QUESTION_WORDS: usize = 4;

/// Decide whether an utterance is worth waking the expensive model for.
///
/// Returns `None` far more often than `Some`, which is the point.
pub fn classify(text: &str) -> Option<Trigger> {
    let normalised = text.trim().to_ascii_lowercase();
    if normalised.is_empty() {
        return None;
    }

    if let Some(phrase) = WAKE_PHRASES
        .iter()
        .find(|phrase| normalised.starts_with(**phrase))
    {
        // Slice the original rather than the lowercased copy, so the model
        // sees what was actually said. `to_ascii_lowercase` only remaps ASCII
        // letters, so byte offsets line up between the two.
        let asked = text.trim()[phrase.len()..].trim_start_matches([' ', ',']).trim();
        return Some(Trigger::Addressed(if asked.is_empty() {
            text.trim().to_string()
        } else {
            asked.to_string()
        }));
    }

    if normalised.split_whitespace().count() < MIN_QUESTION_WORDS {
        return None;
    }

    let ends_in_question_mark = normalised.ends_with('?');
    let opens_interrogatively = INTERROGATIVES
        .iter()
        .any(|word| normalised.starts_with(word));

    if ends_in_question_mark || opens_interrogatively {
        return Some(Trigger::Question(text.trim().to_string()));
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_a_plain_question() {
        assert_eq!(
            classify("How would you design a rate limiter?"),
            Some(Trigger::Question(
                "How would you design a rate limiter?".to_string()
            ))
        );
    }

    #[test]
    fn catches_a_question_whisper_did_not_punctuate() {
        // whisper drops the question mark often enough that relying on it
        // alone would miss a good share of real questions.
        assert!(matches!(
            classify("tell me about a time you disagreed with a colleague"),
            Some(Trigger::Question(_))
        ));
    }

    #[test]
    fn ignores_ordinary_statements() {
        assert_eq!(classify("The connection pool is exhausted."), None);
        assert_eq!(classify("I refactored the auth module this morning."), None);
    }

    #[test]
    fn ignores_short_interjections() {
        // "What?" is someone asking you to repeat yourself.
        assert_eq!(classify("What?"), None);
        assert_eq!(classify("Sorry, what?"), None);
    }

    #[test]
    fn recognises_being_addressed_and_strips_the_wake_phrase() {
        assert_eq!(
            classify("Hey jay, what am I missing here"),
            Some(Trigger::Addressed("what am I missing here".to_string()))
        );
    }

    #[test]
    fn a_bare_wake_phrase_still_counts() {
        assert!(matches!(classify("hey jay"), Some(Trigger::Addressed(_))));
    }

    #[test]
    fn ignores_empty_and_whitespace() {
        assert_eq!(classify(""), None);
        assert_eq!(classify("   "), None);
    }
}
