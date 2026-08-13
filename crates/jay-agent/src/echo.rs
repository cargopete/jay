//! The other person's voice, coming back through your microphone.
//!
//! Without headphones, a question leaves your speakers, crosses the desk and
//! arrives at the microphone, so it is captured on both channels and one copy
//! is blamed on you. Reproduced here exactly, from a real session:
//!
//! ```text
//! [00:11] them: Given a two-dimensional grid of ones and zeros, find the largest island by area.
//! [00:11] you:  Given a two-dimensional grid of ones and zeros, find the largest island by area.
//! ```
//!
//! Wearing headphones remains the correct fix, because this is a room and not a
//! tuning problem. But advice is not a mechanism, and the cost of the room
//! winning is not cosmetic: the duplicate is spent as context, and a model
//! reading it sees the candidate parroting the interviewer word for word.
//!
//! Ordering cannot be relied on. The two lines above are timestamped one second
//! apart, and in a later session the *microphone* copy arrived first, because
//! which transcript finishes first depends on utterance length rather than on
//! when the sound happened. So this looks both ways: a microphone line matching
//! a recent system line is dropped before it is shown, and a system line
//! matching a recent microphone line retracts the copy already recorded.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far apart the two copies of one sentence may *start*.
///
/// Compared against when the audio began, not when whisper finished with it,
/// and that distinction is what makes a tight window safe. Sound crosses a desk
/// in about three milliseconds, so an echo and its original start together; all
/// this has to absorb is the two segmenters disagreeing slightly about where
/// speech began.
///
/// Tight on purpose, because the false positive is expensive. Restating the
/// question back to the interviewer is *good interview practice* — "so, to
/// confirm, we want the largest island by area" — and it is word-for-word
/// similar to what was just asked. The only thing separating it from an echo is
/// that a person cannot begin restating a sentence before the sentence has been
/// said. Two seconds keeps that distinction; four gave it away.
pub const WINDOW: Duration = Duration::from_secs(2);

/// How alike two lines must be, as a fraction of the shorter one's words.
///
/// Not 1.0. The two channels are transcribed from different audio — one has
/// crossed a room and come back — so they routinely differ in punctuation and
/// the odd word.
pub const SIMILARITY: f64 = 0.8;

/// Below this, a match means nothing.
///
/// "Yeah", "okay" and "mm-hm" are said by both people constantly and are
/// identical every time, and so are whole phrases: two people can both say
/// "okay, yeah, that sounds right" within a second without a speaker being
/// involved at all.
///
/// Set one above [`context::FILLER_MAX_WORDS`](crate::context), so the two
/// mechanisms meet rather than overlap. Anything shorter is already dropped as
/// filler before it costs anything, which leaves this free to care only about
/// sentences long enough that a word-for-word repeat is not a coincidence.
pub const MIN_WORDS: usize = 8;

/// One line as the guard remembers it.
struct Seen {
    at: Instant,
    label: String,
    words: Vec<String>,
}

/// Notices the same sentence arriving twice on two channels.
///
/// Holds only the last few seconds, so it costs nothing to keep for a whole
/// session.
#[derive(Default)]
pub struct EchoGuard {
    seen: VecDeque<Seen>,
}

impl EchoGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Has this line already arrived on the *other* channel?
    ///
    /// Same channel does not count: a person repeating themselves is a person
    /// repeating themselves, and jay is not in the business of editing that.
    pub fn is_echo(&self, at: Instant, label: &str, text: &str) -> bool {
        self.matching(at, label, text).is_some()
    }

    /// The remembered line this one duplicates, if any.
    fn matching(&self, at: Instant, label: &str, text: &str) -> Option<&Seen> {
        let words = normalise(text);
        if words.len() < MIN_WORDS {
            return None;
        }
        self.seen.iter().find(|seen| {
            seen.label != label
                && at.duration_since(seen.at) <= WINDOW
                && similarity(&seen.words, &words) >= SIMILARITY
        })
    }

    /// Record a line that was kept, and forget anything now out of the window.
    pub fn remember(&mut self, at: Instant, label: &str, text: &str) {
        while let Some(front) = self.seen.front() {
            if at.duration_since(front.at) > WINDOW {
                self.seen.pop_front();
            } else {
                break;
            }
        }
        self.seen.push_back(Seen {
            at,
            label: label.to_string(),
            words: normalise(text),
        });
    }

    /// The line in `history` this one makes redundant, newest first.
    ///
    /// For the case where the microphone copy won the race and is already in
    /// the transcript when the system copy arrives. Returns an index into
    /// `history`, whose entries carry their `"label: text"` prefix.
    ///
    /// Only ever points at a *microphone* line: when the two disagree, the
    /// channel that did not cross a room is the one to keep.
    pub fn stale_copy(&self, history: &[String], mic_label: &str, text: &str) -> Option<usize> {
        let words = normalise(text);
        if words.len() < MIN_WORDS {
            return None;
        }
        history.iter().rposition(|line| {
            let Some((label, body)) = line.split_once(": ") else {
                return false;
            };
            label == mic_label && similarity(&normalise(body), &words) >= SIMILARITY
        })
    }
}

/// Words, lowercased, stripped of punctuation. Order is not kept because the
/// comparison does not use it.
fn normalise(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_ascii_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Shared words as a fraction of the shorter line.
///
/// Over the shorter rather than the union, because one channel routinely
/// truncates: the microphone copy often loses the first word or two while the
/// VAD is still making its mind up, and that is still an echo.
fn similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    // Multiset intersection: a sentence that says "the" three times should not
    // match one that says it once, three times over.
    let mut remaining: Vec<&String> = b.iter().collect();
    let mut shared = 0usize;
    for word in a {
        if let Some(pos) = remaining.iter().position(|w| *w == word) {
            remaining.swap_remove(pos);
            shared += 1;
        }
    }
    shared as f64 / a.len().min(b.len()) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISLAND: &str =
        "Given a two-dimensional grid of ones and zeros, find the largest island by area.";

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn the_microphone_copy_of_an_interviewer_question_is_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, "them", ISLAND);
        assert!(guard.is_echo(at(base, 1), "you", ISLAND));
    }

    /// The two channels are transcribed from different audio, so they differ.
    #[test]
    fn survives_the_transcriber_disagreeing_with_itself() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, "them", ISLAND);
        assert!(guard.is_echo(
            at(base, 1),
            "you",
            "given a two dimensional grid of ones and zeros find the largest island by area"
        ));
        // The microphone often misses the opening word or two while the VAD
        // makes its mind up.
        assert!(guard.is_echo(
            at(base, 1),
            "you",
            "grid of ones and zeros, find the largest island by area."
        ));
    }

    #[test]
    fn a_person_repeating_themselves_is_not_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, "them", ISLAND);
        assert!(!guard.is_echo(at(base, 1), "them", ISLAND));
    }

    /// Both people say these, constantly, and identically. Below [`MIN_WORDS`]
    /// nothing is an echo however exact the match, because the alternative is
    /// silently editing a conversation on the strength of five common words.
    #[test]
    fn agreement_is_not_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        for phrase in [
            "Okay, yeah, that sounds right.",
            "Yeah, exactly.",
            "Right, that makes sense to me.",
        ] {
            guard.remember(base, "them", phrase);
            assert!(
                !guard.is_echo(at(base, 1), "you", phrase),
                "treated ordinary agreement as an echo: {phrase:?}"
            );
        }
    }

    #[test]
    fn a_different_answer_to_the_same_question_is_not_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, "them", ISLAND);
        assert!(!guard.is_echo(
            at(base, 1),
            "you",
            "Right, so I would flood fill from every unvisited land cell and keep the biggest."
        ));
    }

    #[test]
    fn the_window_closes() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, "them", ISLAND);
        // Asked again a minute later, genuinely, because the candidate lost it.
        assert!(!guard.is_echo(at(base, 60), "you", ISLAND));
    }

    /// Restating the question back is good practice and must survive. It is
    /// separated from an echo only by the fact that a person cannot start
    /// saying a sentence before it has been said to them.
    #[test]
    fn repeating_the_question_back_to_confirm_is_not_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, "them", ISLAND);
        assert!(!guard.is_echo(at(base, 3), "you", ISLAND));
    }

    /// The microphone copy arriving first, which is what happened in the
    /// session that prompted all this.
    #[test]
    fn a_copy_already_in_the_transcript_can_be_retracted() {
        let guard = EchoGuard::new();
        let history = vec![
            "them: So, next question.".to_string(),
            format!("you: {ISLAND}"),
        ];
        assert_eq!(guard.stale_copy(&history, "you", ISLAND), Some(1));
        // And it never points at the interviewer's own line.
        assert_eq!(
            guard.stale_copy(&["them: ".to_string() + ISLAND], "you", ISLAND),
            None
        );
    }

    #[test]
    fn repeated_words_do_not_inflate_a_match() {
        let a = normalise("the the the the the the");
        let b = normalise("the quick brown fox jumps over");
        assert!(similarity(&a, &b) < SIMILARITY);
    }
}
