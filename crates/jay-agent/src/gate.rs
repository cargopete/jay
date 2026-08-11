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
//!
//! # What a real transcript taught this module
//!
//! The test corpus at the bottom is taken verbatim from a recorded interview
//! where a commercial tool of this kind performed badly. It failed in one
//! specific way, over and over: it treated every question as a question worth
//! answering, and so kept producing help through three solid minutes of
//! scheduling ("do you see the updated invitation?", "would you like to start
//! earlier?"). Detecting a question is easy. Detecting a question *worth
//! spending twelve seconds and twenty cents on* is the actual problem.

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

/// Markers of a question about logistics, scheduling or the call itself.
///
/// Every one of these is drawn from a real transcript in which a tool of this
/// kind kept offering technical help through the goodbyes. A question
/// containing any of them is social or administrative, and answering it is
/// worse than saying nothing: it costs money and it puts text in front of
/// someone who is trying to agree a meeting time.
const SMALL_TALK: &[&str] = &[
    "invitation", "invite", "calendar", "reschedule", "schedule", "slot",
    "earlier", "later today", "this hour", "one hour", "your time", "my time",
    "see you", "hear me", "see my screen", "share my screen", "audio",
    "sound ok", "thanks a lot", "have a great day", "enjoy the rest",
    "feedback", "get back to you", "next week", "email",
    // Confirmations about the call itself. Kept specific rather than matching
    // a bare "today", which would swallow legitimate questions about work.
    "another interview", "for today", "i assume we", "setup still",
];

/// Minimum words before a question is worth escalating.
///
/// "What?" is a request for repetition, not a question worth an answer, and
/// escalating on it would be both expensive and irritating.
const MIN_QUESTION_WORDS: usize = 4;

/// Who said the thing being classified.
///
/// This matters more than it looks. In an interview or a pairing session the
/// questions worth answering come from the *other* person; your own speech is
/// the record of what you have already covered, and a question you mutter to
/// yourself while thinking ("but what do we do, do we ask them to
/// authenticate?" — taken from a real transcript) is thinking aloud, not a
/// request for help. Treating both identically is the single largest source of
/// noise in a tool like this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    /// You. Your own microphone.
    You,
    /// Everyone else, arriving over system audio.
    Them,
}

/// Decide whether an utterance is worth waking the expensive model for.
///
/// Speaker-blind: kept for the single-channel case and for tests. Prefer
/// [`classify_from`] when the channel is known.
pub fn classify(text: &str) -> Option<Trigger> {
    classify_from(text, Speaker::Them)
}

/// As [`classify`], but knowing who spoke.
///
/// Returns `None` far more often than `Some`, which is the point.
pub fn classify_from(text: &str, speaker: Speaker) -> Option<Trigger> {
    let normalised = text.trim().to_ascii_lowercase();
    if normalised.is_empty() {
        return None;
    }

    // Being addressed by name outranks everything, including the small-talk
    // filter: if you say jay's name, you meant it.
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

    // Past this point only the other person's questions are acted on. Yours
    // are you thinking aloud; jay hears them, records them as context, and
    // keeps its mouth shut unless you say its name.
    if speaker == Speaker::You {
        return None;
    }

    if normalised.split_whitespace().count() < MIN_QUESTION_WORDS {
        return None;
    }

    if is_small_talk(&normalised) {
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

/// Is this about the meeting rather than about the subject of the meeting?
fn is_small_talk(normalised: &str) -> bool {
    SMALL_TALK.iter().any(|marker| normalised.contains(marker))
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

    /// Verbatim from a real interview recording, in which a commercial tool of
    /// this kind kept offering technical help throughout. Every line here is a
    /// grammatical question and not one of them wants an answer from jay.
    #[test]
    fn declines_the_scheduling_chat_that_sank_the_competition() {
        for line in [
            "Do you see the updated invitation?",
            "would you like, are you, like, committed to this particular hour \
             or would you like to start, for example, earlier?",
            "So we have one hour break?",
            "if you can move it one your time, that would be, uh, two?",
            "I assume we have another interview setup still for today, right?",
        ] {
            assert_eq!(classify(line), None, "should have declined: {line}");
        }
    }

    /// From the same recording: the questions that genuinely wanted help.
    #[test]
    fn still_escalates_the_technical_questions_from_the_same_call() {
        for line in [
            "How would you handle authentication for the update endpoint?",
            "What happens if the user isn't logged in at all?",
            "Can you walk me through the space complexity of that approach?",
        ] {
            assert!(
                matches!(classify(line), Some(Trigger::Question(_))),
                "should have escalated: {line}"
            );
        }
    }

    #[test]
    fn your_own_thinking_aloud_is_not_a_request_for_help() {
        // Verbatim from the real transcript: the candidate reasoning out loud
        // mid-answer. The competing tool treated this as a question and
        // answered it, which is help arriving on top of the person it is
        // meant to be helping.
        assert_eq!(
            classify_from(
                "But what do we do? We ask them to authenticate or is there \
                 something like, uh, a different idea?",
                Speaker::You
            ),
            None
        );
    }

    #[test]
    fn the_same_question_from_them_does_escalate() {
        assert!(matches!(
            classify_from(
                "How would you make sure only the creator can update the link?",
                Speaker::Them
            ),
            Some(Trigger::Question(_))
        ));
    }

    #[test]
    fn you_can_still_address_jay_by_name() {
        // Speaker gating must not stop you asking jay directly.
        assert!(matches!(
            classify_from("hey jay, what am I forgetting", Speaker::You),
            Some(Trigger::Addressed(_))
        ));
    }

    #[test]
    fn being_addressed_beats_the_small_talk_filter() {
        // "schedule" is a small-talk marker, but if you say jay's name you
        // meant to ask, and second-guessing that would be maddening.
        assert!(matches!(
            classify("hey jay, how should I schedule these workers?"),
            Some(Trigger::Addressed(_))
        ));
    }
}
