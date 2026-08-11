//! The part that decides whether to speak, and what to say.
//!
//! Two tiers, deliberately unequal:
//!
//! 1. [`gate`] runs on every utterance and is pure rules. It costs nothing.
//! 2. [`claude`] runs only when the gate escalates, and costs real money.
//!
//! The split is the whole design. Measured on this machine, one `claude -p`
//! call costs $0.0254 cold and $0.0033 warm regardless of how trivial the
//! question is, because the CLI carries ~29,000 tokens of its own preamble.
//! A model in the gate would therefore cost roughly $0.40 an hour to answer
//! yes or no, which is why there isn't one.

use std::time::Duration;

pub mod brief;
pub mod claude;
pub mod gate;
pub mod screen;

/// What jay is being used for. Changes what it is asked, not what it can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A live algorithmic interview: LeetCode-shaped, you are writing code.
    ///
    /// The tightest mode there is. You are typing and talking at the same
    /// time, so anything longer than a phrase is a distraction. What actually
    /// helps is the name of the approach, the complexity, and the constraint
    /// that breaks the obvious solution — the sort of thing that turns "I'll
    /// recurse" into "recursion blows the stack on a large grid, go
    /// iterative with an explicit stack".
    Coding,
    /// A live system design interview.
    ///
    /// Slightly more room than [`Coding`](Mode::Coding), because the unit of
    /// value is a missing component or an unnamed tradeoff rather than a
    /// one-line insight. Still speech, still no code.
    SystemDesign,
    /// Mock interview practice, run the way a real debrief runs: what your
    /// attempt missed, then the full worked answer.
    ///
    /// Real code for an algorithmic problem, a real component diagram for a
    /// design one. This is the one mode that hands over complete solutions,
    /// and it is the right one to do it in: a model answer you compare against
    /// your own attempt is the fastest way to get better, which is why every
    /// algorithms book prints them — after the exercise.
    Rehearsal,
    /// Live pairing or coaching: concrete, short, opinionated.
    Pairing,
    /// Proactive dev assistant: reacting to a red test or a stack trace.
    Dev,
}

impl Mode {
    /// Appended to Claude Code's own system prompt for this mode.
    ///
    /// Kept short deliberately. Every token here is paid on every call, and
    /// the models follow a brief instruction as well as a long one.
    ///
    /// Every prompt here is written around one measured fact: a suggestion
    /// takes twelve to twenty seconds to arrive. By then the person has
    /// already started answering. A tool that races them and loses is worse
    /// than useless — it puts a competing paragraph in their eyeline mid
    /// sentence. So jay is told, in every mode, not to restate what has
    /// already been covered. Arriving late is only a problem if you were
    /// trying to be first.
    pub fn system_prompt(self) -> &'static str {
        match self {
            Mode::Coding => {
                "The person you are helping is in a live algorithmic coding \
                 interview. They are typing and talking at once, so brevity is \
                 everything: under forty words, plain speech, no formatting. \
                 Do not write their solution — they are the one being assessed \
                 and a pasted implementation helps nobody. Give the shape \
                 instead: name the approach or data structure, the time and \
                 space complexity said as you would say it aloud, and the one \
                 constraint that breaks the naive version (recursion depth on \
                 large inputs, an overflow, an off-by-one at the boundary, the \
                 empty case). If they already have the right approach, say the \
                 edge case they have not mentioned, or reply: covered."
            }
            Mode::SystemDesign => {
                "The person you are helping is in a live system design \
                 interview and is already speaking. Everything must be sayable \
                 aloud immediately: plain spoken sentences, no code, no \
                 markdown, no headings, no bullet lists, no formatting. At \
                 most three points, most valuable first, under sixty words. \
                 Prefer one excellent point to three adequate ones. If they \
                 have covered it well, reply with the single word: covered. \
                 Give the boring solution teams actually ship, not the \
                 sophisticated one: a unique constraint rather than a \
                 distributed lock, a cache rather than a custom protocol. \
                 Interviewers mark people down for reaching past the problem, \
                 and a candidate who names the simple mechanism and says why \
                 it is enough sounds more senior than one who reaches for the \
                 impressive answer."
            }
            Mode::Rehearsal => {
                "You are running the debrief after a mock interview. The \
                 person has already attempted this and wants to compare \
                 against a good answer.\n\nStart with what their attempt \
                 missed or got wrong, specifically, quoting them where it \
                 helps. Then give the full worked answer.\n\nFor an \
                 algorithmic problem: the approach in a sentence, then \
                 complete, idiomatic, compiling code with the invariant that \
                 makes it correct named in a comment, then time and space \
                 complexity, then the edge cases a first attempt misses.\n\n\
                 For a design problem: an ASCII component diagram showing the \
                 data flow, then each component in a line, then the two or \
                 three decisions that actually matter and what was traded away \
                 for each. Say which parts you would cut first under time \
                 pressure — knowing what is load-bearing is most of the skill."
            }
            Mode::Pairing => {
                "You are the second engineer in a pairing session. Be concrete, \
                 be brief, and have an opinion. Say the useful thing, not the \
                 complete thing."
            }
            Mode::Dev => {
                "You are watching a developer work. Something just went wrong. \
                 Say what is most likely responsible and what to check first. \
                 Be specific and brief. Say plainly when you cannot tell."
            }
        }
    }
}

/// Appended to every mode's prompt. See [`Mode::system_prompt`] for why.
pub const LATE_ARRIVAL: &str = " You are always reading a conversation already \
    in progress: by the time this reaches them they will have started \
    answering. Read what they have already said and do not repeat it back. \
    Give only what is missing, wrong, or worth adding. If they have covered it \
    well, say so in one line and stop.";

/// What came back, with what it cost.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub text: String,
    /// Reported by the CLI. Worth surfacing: this is the number that decides
    /// whether the whole idea is viable.
    pub cost_usd: f64,
    pub latency: Duration,
    pub model: String,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("could not run the claude CLI: {0}")]
    Spawn(String),
    #[error("claude returned an error: {0}")]
    Cli(String),
    #[error("could not read the response: {0}")]
    Parse(String),
    #[error("screen capture failed: {0}")]
    Screen(String),
    #[error("assembling the brief: {0}")]
    Brief(String),
}

pub type Result<T> = std::result::Result<T, AgentError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_a_distinct_system_prompt() {
        let prompts = [
            Mode::Rehearsal.system_prompt(),
            Mode::Pairing.system_prompt(),
            Mode::Dev.system_prompt(),
        ];
        assert_ne!(prompts[0], prompts[1]);
        assert_ne!(prompts[1], prompts[2]);
    }

    /// The line the whole design rests on, kept honest by a test.
    ///
    /// Practice hands over the complete answer, because comparing your attempt
    /// against a good one is how you improve. The live modes never do: writing
    /// someone's solution while an employer assesses them misrepresents them
    /// to the person making the decision, and that is a different product.
    #[test]
    fn only_practice_hands_over_a_full_solution() {
        let rehearsal = Mode::Rehearsal.system_prompt().to_lowercase();
        assert!(rehearsal.contains("complete, idiomatic, compiling code"));
        assert!(rehearsal.contains("ascii component diagram"));

        let live = Mode::Coding.system_prompt().to_lowercase();
        assert!(live.contains("do not write their solution"));
        assert!(!live.contains("compiling code"));

        // Each live mode forbids handing over the artefact in its own words.
        assert!(Mode::SystemDesign.system_prompt().to_lowercase().contains("no code"));

        // And both stay short enough to say out loud.
        for mode in [Mode::Coding, Mode::SystemDesign] {
            let prompt = mode.system_prompt().to_lowercase();
            assert!(
                prompt.contains("under forty words") || prompt.contains("under sixty words"),
                "{mode:?} should cap its length"
            );
        }
    }
}
