//! Choosing which of the conversation to send.
//!
//! A forty-minute interview is several hundred transcript lines. Sending a
//! dozen of them, as jay first did, means the model reasons about the last
//! ninety seconds and nothing else. Sending all of them wastes a large amount
//! of the budget on "Yeah. Okay. Right."
//!
//! So: budget in *words* rather than lines, because line lengths vary by an
//! order of magnitude, and drop the lines that carry nothing before spending
//! any of that budget on them. The filtered lines are real ones, from a
//! recorded interview.
//!
//! Note this is the opposite conclusion to the one for the standing brief,
//! where more context measurably made answers worse. The difference is that
//! everything in the conversation is about the thing at hand, whereas most of
//! a 181-project memory index is not.

/// Words of conversation to send with a suggestion.
///
/// Roughly 1,600 tokens: a substantial slice of the discussion, and small
/// beside the ~29,000-token preamble the CLI carries anyway.
pub const WORD_BUDGET: usize = 1_200;

/// Single words that carry nothing on their own.
///
/// Single words only: the check runs word by word, so a multi-word entry here
/// would never match. Phrases live in [`FILLER_PHRASES`].
const FILLER: &[&str] = &[
    "yeah", "yes", "yep", "yup", "no", "ok", "okay", "right", "sure", "mhm",
    "mm", "hmm", "uh", "um", "er", "erm", "so", "well", "and", "but", "all",
    "alright", "exactly", "thanks", "cheers", "sorry", "true", "indeed",
    "cool", "nice", "got", "it", "i", "see", "of", "course", "you",
    // Interjections, all of which appear in the recorded transcript.
    "huh", "hm", "eh", "ah", "oh", "wow", "hey", "pardon", "what", "mate",
    "bye", "by", "cheers", "great", "day", "have", "a",
];

/// Whole utterances that are pure acknowledgement.
///
/// Checked against the entire line, after normalising, because their words are
/// too useful individually to put in [`FILLER`].
const FILLER_PHRASES: &[&str] = &[
    "makes sense", "that makes sense", "i see", "got it", "of course",
    "thank you", "thanks a lot", "fair enough", "sounds good",
];

/// Longest an utterance can be and still be dismissed as acknowledgement.
///
/// "Okay. Okay. Yeah. All right. Yeah, that makes sense." is six words of
/// nothing; a seventh word usually means content arrived.
const FILLER_MAX_WORDS: usize = 7;

/// Does this line carry anything worth paying for?
///
/// Deliberately conservative: dropping a line that mattered is far worse than
/// keeping one that did not, so anything with a word outside the filler list
/// survives.
pub fn is_noise(line: &str) -> bool {
    // Strip the "you: " / "them: " prefix the transcript carries.
    let body = line.split_once(": ").map_or(line, |(_, rest)| rest);

    let words: Vec<String> = body
        .split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_ascii_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();

    if words.is_empty() {
        return true;
    }
    if words.len() > FILLER_MAX_WORDS {
        return false;
    }

    let joined = words.join(" ");
    if FILLER_PHRASES.contains(&joined.as_str()) {
        return true;
    }

    words.iter().all(|word| FILLER.contains(&word.as_str()))
}

/// The most recent conversation that fits in `word_budget`, noise removed.
///
/// Takes from the end, because the recent turns are the ones being reasoned
/// about, then restores chronological order.
pub fn window(lines: &[String], word_budget: usize) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut words = 0;

    for line in lines.iter().rev() {
        if is_noise(line) {
            continue;
        }
        let cost = line.split_whitespace().count();
        if words + cost > word_budget && !kept.is_empty() {
            break;
        }
        words += cost;
        kept.push(line.clone());
    }

    kept.reverse();
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_the_acknowledgements_from_a_real_transcript() {
        for line in [
            "you: Okay. Okay. Yeah. All right.",
            "them: Yeah. Yeah.",
            "you: Um.",
            "you: Uh.",
            "you: Huh?",
            "you: That makes sense.",
            "them: Of course.",
        ] {
            assert!(is_noise(line), "should have been dropped: {line}");
        }
    }

    /// The filter errs towards keeping, on purpose.
    #[test]
    fn keeps_a_short_line_that_still_answers_something() {
        // "Yes. Later today. Yeah." reads like acknowledgement and is not: it
        // commits to a time. Dropping a line that mattered is far worse than
        // paying four words to keep one that did not.
        assert!(!is_noise("them: Yes. Later today. Yeah."));
    }

    #[test]
    fn keeps_anything_with_content() {
        for line in [
            "them: Given a two-dimensional grid of ones and zeros, count the islands.",
            "you: I'd do a depth first search from each unvisited land cell.",
            // Short, but the words are not filler.
            "you: use an explicit stack",
        ] {
            assert!(!is_noise(line), "should have been kept: {line}");
        }
    }

    #[test]
    fn a_long_hedging_answer_is_not_noise() {
        // From the real transcript. It is mostly filler by feel, but it
        // contains the candidate's actual reasoning and dropping it would
        // lose the thing the debrief needs to quote.
        let line = "you: Um. I think that I'm just thinking because one place \
                    where that can happen is the API gateway, but it can be on \
                    the write service as well, but.";
        assert!(!is_noise(line));
    }

    #[test]
    fn window_takes_the_most_recent_and_keeps_order() {
        let lines: Vec<String> = (0..50)
            .map(|i| format!("them: line number {i} with some words in it"))
            .collect();
        let kept = window(&lines, 40);
        assert!(kept.len() < 50);
        assert!(kept.last().unwrap().contains("line number 49"));
        // chronological
        let first: usize = kept[0]
            .split_whitespace()
            .nth(3)
            .unwrap()
            .parse()
            .unwrap();
        assert!(first < 49);
    }

    #[test]
    fn window_keeps_one_line_even_if_it_blows_the_budget() {
        // A single long utterance is still better than sending nothing.
        let lines = vec!["them: ".to_string() + &"word ".repeat(500)];
        assert_eq!(window(&lines, 10).len(), 1);
    }

    #[test]
    fn window_of_nothing_is_nothing() {
        assert!(window(&[], WORD_BUDGET).is_empty());
        assert!(window(&["you: yeah".to_string()], WORD_BUDGET).is_empty());
    }
}
