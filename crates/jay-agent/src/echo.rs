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

/// Fewest words a *contained* fragment needs. See [`EchoGuard::matching`].
///
/// Lower than [`MIN_WORDS`], because containment is carrying evidence that a
/// bare similarity score is not: this fragment was spoken entirely inside
/// another channel's utterance, and its words all appear in it. Four still
/// keeps "yeah, okay, right" out of it, which two people say over each other
/// all day without a loudspeaker being involved.
pub const CONTAINED_MIN_WORDS: usize = 4;

/// How much of a contained fragment must appear in the utterance holding it.
///
/// Stricter than [`SIMILARITY`]. A fragment is short, so a single coincidental
/// word is a larger fraction of it, and the whole point of the rule is that
/// the fragment is *made of* the other channel's words.
pub const CONTAINED_SIMILARITY: f64 = 0.9;

/// How much longer the parent must be than the fragment it contains.
///
/// A fragment is a *piece* of something. Two utterances of similar length that
/// happen to overlap and read alike are not a whole and a part, they are two
/// people saying the same short thing at the same time — which is what
/// "Okay, yeah, that sounds right." is, and both of them said it. Without this
/// the containment rule called that the room.
///
/// Genuine same-length duplicates are the alignment rule's job, and it has an
/// eight-word floor for exactly this reason. This keeps the two rules from
/// reaching into each other's territory.
const CONTAINED_LENGTH_RATIO: usize = 2;

/// Slack at each end of a containment test.
///
/// The two channels run separate VADs over separately-resampled audio and
/// disagree by a frame or two about where speech begins and ends. Without
/// this, a fragment that starts 40 ms before its parent is not contained and
/// the rule does nothing.
const EDGE_SLACK: Duration = Duration::from_secs(1);

/// One line as the guard remembers it.
struct Seen {
    at: Instant,
    /// When the speech stopped. The pair is a span, and the span is what makes
    /// the containment rule possible.
    until: Instant,
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
    pub fn is_echo(&self, at: Instant, spoken: Duration, label: &str, text: &str) -> bool {
        self.matching(at, spoken, label, text).is_some()
    }

    /// The remembered line this one duplicates, if any.
    ///
    /// Two rules, because the room produces two shapes.
    ///
    /// **Alignment.** Both channels heard one sentence and both transcribed it
    /// whole, so the copies start together and read alike. This is the
    /// original rule and it handles the clean case.
    ///
    /// **Containment.** The channels disagreed about where the sentences were.
    /// A 75-minute meeting opened with one 25-second system utterance against
    /// three short microphone fragments, each sitting inside its span and made
    /// of its words. Not one of them started more than two seconds from the
    /// system copy — they all started inside it — but the alignment rule still
    /// missed two of the three, and the third only because a later retraction
    /// happened to catch it.
    ///
    /// Containment is a better discriminator than loudness, which is what this
    /// was going to be. Restating a question back to the interviewer is good
    /// practice and word-for-word similar to what was asked, and a person
    /// cannot begin restating a sentence until it has been said — so a genuine
    /// restatement starts *after* its source ends and is never contained. The
    /// rule protects that case by construction rather than by a threshold
    /// somebody has to tune.
    fn matching(
        &self,
        at: Instant,
        spoken: Duration,
        label: &str,
        text: &str,
    ) -> Option<&Seen> {
        let words = normalise(text);
        let until = at + spoken;
        self.seen.iter().find(|seen| {
            if seen.label == label {
                return false;
            }
            let aligned = words.len() >= MIN_WORDS
                && at.duration_since(seen.at) <= WINDOW
                && similarity(&seen.words, &words) >= SIMILARITY;
            let contained = words.len() >= CONTAINED_MIN_WORDS
                && seen.words.len() >= words.len() * CONTAINED_LENGTH_RATIO
                && at + EDGE_SLACK >= seen.at
                && until <= seen.until + EDGE_SLACK
                && similarity(&words, &seen.words) >= CONTAINED_SIMILARITY;
            aligned || contained
        })
    }

    /// Record a line that was kept, and forget anything now out of the window.
    ///
    /// Retained for the length of the utterance plus the window, not the
    /// window alone: a 25-second sentence has to still be remembered when the
    /// fragment that overlapped its final second comes to be judged.
    pub fn remember(&mut self, at: Instant, spoken: Duration, label: &str, text: &str) {
        while let Some(front) = self.seen.front() {
            if at.duration_since(front.until) > WINDOW {
                self.seen.pop_front();
            } else {
                break;
            }
        }
        self.seen.push_back(Seen {
            at,
            until: at + spoken,
            label: label.to_string(),
            words: normalise(text),
        });
    }

    /// Every line in `history` this one makes redundant, newest first.
    ///
    /// For the case where the microphone copy won the race and is already in
    /// the transcript when the system copy arrives. Returns indices into
    /// `history`, whose entries carry their `"label: text"` prefix, in
    /// descending order so a caller can remove them without invalidating the
    /// ones it has not reached yet.
    ///
    /// **Plural, and it used to be singular.** `rposition` found the last
    /// match and the caller removed one line, which is right up until one long
    /// system utterance corresponds to several microphone fragments — which is
    /// the ordinary case, since the two channels segment independently. In a
    /// real meeting that left two of three fragments in the transcript,
    /// attributed to somebody who had not spoken.
    ///
    /// Only ever points at *microphone* lines: when the two disagree, the
    /// channel that did not cross a room is the one to keep.
    pub fn stale_copies(&self, history: &[String], mic_label: &str, text: &str) -> Vec<usize> {
        let words = normalise(text);
        if words.len() < MIN_WORDS {
            return Vec::new();
        }
        history
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, line)| {
                let Some((label, body)) = line.split_once(": ") else {
                    return false;
                };
                if label != mic_label {
                    return false;
                }
                let fragment = normalise(body);
                // Either copy may be the shorter one: a fragment is judged on
                // how much of *it* appears in the whole, the whole on how much
                // of it appears in the fragment. `similarity` divides by the
                // shorter, so one call answers both.
                fragment.len() >= CONTAINED_MIN_WORDS
                    && similarity(&fragment, &words) >= SIMILARITY
            })
            .map(|(index, _)| index)
            .collect()
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

    /// The shape that got through a real 75-minute meeting: one long system
    /// utterance, several short microphone fragments sitting inside it, each
    /// made of its words. Rewritten rather than quoted — the other people on
    /// that call did not agree to be published — but the geometry is theirs:
    /// a 25-second parent starting at zero, fragments at 0-3, 4-7 and 7-13.
    ///
    /// Before containment, two of these three survived and were archived as
    /// things the person running the recording had said. One of them is seven
    /// words, below `MIN_WORDS`, so the old rule never looked at it at all.
    #[test]
    fn fragments_of_one_long_utterance_are_all_the_room() {
        const PARENT: &str = "Every time you write you read it back from that cache. \
             Even so, I think we solved the problem already. Because the read after \
             write, you would expect that to succeed every time.";
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, Duration::from_secs(25), "them", PARENT);

        for (from, secs, fragment) in [
            (0, 3, "Every time you write you read it back from that cache."),
            (4, 3, "Even so, I think we solved the problem."),
            (7, 6, "Because the read after write, you would expect that to succeed."),
        ] {
            assert!(
                guard.is_echo(at(base, from), Duration::from_secs(secs), "you", fragment),
                "left in the transcript as something you said: {fragment}"
            );
        }
    }

    /// The case containment exists to protect, and the reason it beats
    /// judging this on loudness. Restating the question back is good practice,
    /// word-for-word similar, and *starts after the question ends* — nobody
    /// can begin repeating a sentence before it has been said.
    #[test]
    fn restating_the_question_afterwards_is_not_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, Duration::from_secs(6), "them", ISLAND);
        assert!(!guard.is_echo(
            at(base, 8),
            Duration::from_secs(5),
            "you",
            "so to confirm, given a grid of ones and zeros, find the largest island by area"
        ));
    }

    /// Two people saying a short thing over each other is not the room, and a
    /// contained fragment has to be long enough that its words being someone
    /// else's is not a coincidence.
    #[test]
    fn a_brief_agreement_inside_someone_elses_sentence_is_left_alone() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(
            base,
            Duration::from_secs(20),
            "them",
            "yeah so the thing about that is we would have to shard it by tenant \
             and then the rebalance cost goes up, right",
        );
        assert!(!guard.is_echo(at(base, 4), Duration::from_secs(1), "you", "yeah right"));
    }

    /// One long utterance, several stale fragments already archived. The
    /// retraction used to remove exactly one of them.
    #[test]
    fn every_stale_fragment_is_retracted_not_merely_the_last() {
        let guard = EchoGuard::new();
        let history = vec![
            "them: something else entirely".to_string(),
            "you: Every time you write you read it back from that cache".to_string(),
            "you: Even so, I think we solved the problem".to_string(),
            "you: Because the read after write, you would expect that to succeed".to_string(),
        ];
        const PARENT: &str = "Every time you write you read it back from that cache. \
             Even so, I think we solved the problem already. Because the read after \
             write, you would expect that to succeed every time.";
        let stale = guard.stale_copies(&history, "you", PARENT);
        assert_eq!(stale, vec![3, 2, 1], "descending, and all of them");
    }

    #[test]
    fn the_microphone_copy_of_an_interviewer_question_is_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, Duration::from_secs(4), "them", ISLAND);
        assert!(guard.is_echo(at(base, 1), Duration::from_secs(4), "you", ISLAND));
    }

    /// The two channels are transcribed from different audio, so they differ.
    #[test]
    fn survives_the_transcriber_disagreeing_with_itself() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, Duration::from_secs(4), "them", ISLAND);
        assert!(guard.is_echo(
            at(base, 1),
            Duration::from_secs(4),
            "you",
            "given a two dimensional grid of ones and zeros find the largest island by area"
        ));
        // The microphone often misses the opening word or two while the VAD
        // makes its mind up.
        assert!(guard.is_echo(
            at(base, 1),
            Duration::from_secs(4),
            "you",
            "grid of ones and zeros, find the largest island by area."
        ));
    }

    #[test]
    fn a_person_repeating_themselves_is_not_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, Duration::from_secs(4), "them", ISLAND);
        assert!(!guard.is_echo(at(base, 1), Duration::from_secs(4), "them", ISLAND));
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
            guard.remember(base, Duration::from_secs(4), "them", phrase);
            assert!(
                !guard.is_echo(at(base, 1), Duration::from_secs(4), "you", phrase),
                "treated ordinary agreement as an echo: {phrase:?}"
            );
        }
    }

    #[test]
    fn a_different_answer_to_the_same_question_is_not_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, Duration::from_secs(4), "them", ISLAND);
        assert!(!guard.is_echo(
            at(base, 1),
            Duration::from_secs(4),
            "you",
            "Right, so I would flood fill from every unvisited land cell and keep the biggest."
        ));
    }

    #[test]
    fn the_window_closes() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, Duration::from_secs(4), "them", ISLAND);
        // Asked again a minute later, genuinely, because the candidate lost it.
        assert!(!guard.is_echo(at(base, 60), Duration::from_secs(4), "you", ISLAND));
    }

    /// Restating the question back is good practice and must survive. It is
    /// separated from an echo only by the fact that a person cannot start
    /// saying a sentence before it has been said to them.
    #[test]
    fn repeating_the_question_back_to_confirm_is_not_an_echo() {
        let base = Instant::now();
        let mut guard = EchoGuard::new();
        guard.remember(base, Duration::from_secs(4), "them", ISLAND);
        assert!(!guard.is_echo(at(base, 3), Duration::from_secs(4), "you", ISLAND));
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
        assert_eq!(guard.stale_copies(&history, "you", ISLAND), vec![1]);
        // And it never points at the interviewer's own line.
        assert_eq!(
            guard.stale_copies(&["them: ".to_string() + ISLAND], "you", ISLAND),
            Vec::<usize>::new()
        );
    }

    #[test]
    fn repeated_words_do_not_inflate_a_match() {
        let a = normalise("the the the the the the");
        let b = normalise("the quick brown fox jumps over");
        assert!(similarity(&a, &b) < SIMILARITY);
    }
}
