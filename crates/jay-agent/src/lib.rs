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

pub mod archive;
pub mod brief;
pub mod claude;
pub mod context;
pub mod gate;
pub mod screen;

/// How much of an answer to give.
///
/// The two are genuinely different tools. Practising with a partner playing
/// interviewer, you usually want the whole thing — the code, the diagram, the
/// capacity numbers — because you are comparing your attempt against a good
/// answer and that is how the comparison happens. But sometimes you want to be
/// nudged rather than told, because being handed the answer too early robs you
/// of the rep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Depth {
    /// The worked answer: real compiling code, a real component diagram.
    #[default]
    Full,
    /// A nudge: the approach, the complexity, the thing you are about to miss.
    ///
    /// Under forty words for coding, sixty for design, no implementation. Use
    /// it when you want to solve it yourself and are stuck rather than lost.
    Hint,
}

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
    pub fn system_prompt(self, depth: Depth) -> &'static str {
        if depth == Depth::Hint {
            return match self {
                Mode::Coding => {
                    "The person is working through an algorithmic problem and \
                     wants a nudge, not the answer. Under forty words, plain \
                     speech, no formatting. Do not write the solution — they \
                     want the rep. Name the approach or data structure, the \
                     time and space complexity, or the one constraint that \
                     breaks the naive version. If they already have it, say \
                     the edge case they have not mentioned, or reply: covered."
                }
                Mode::SystemDesign => {
                    "The person is working through a design problem and wants \
                     a nudge, not the answer. Under sixty words, plain speech, \
                     no code, no formatting. Give the missing component, the \
                     unnamed tradeoff, or the failure mode they have not \
                     considered. Prefer one excellent point to three adequate \
                     ones. Give the boring solution teams actually ship: a \
                     unique constraint rather than a distributed lock. If they \
                     have covered it, reply: covered."
                }
                _ => Mode::Coding.system_prompt(Depth::Hint),
            };
        }

        match self {
            Mode::Coding => {
                "The person is practising algorithmic interview questions with \
                 a partner playing interviewer. Give the solution.\n\nOpen \
                 with the approach in one sentence, so they can say it aloud \
                 before any code exists. Then complete, idiomatic, compiling \
                 code — Rust unless they say otherwise — with the invariant \
                 that makes it correct named in a comment rather than left \
                 implicit. Then the time and space complexity, phrased as they \
                 would say it to an interviewer. Then the edge cases a first \
                 attempt misses: the empty input, the single element, the \
                 boundary, and whatever is specific to this problem.\n\nIf \
                 the transcript shows they have already started, say what \
                 their approach gets wrong or misses before giving yours."
            }
            Mode::SystemDesign => {
                "The person is practising system design questions with a \
                 partner playing interviewer. Give the answer.\n\nOpen with \
                 the numbers that drive the design — request rates, read to \
                 write ratio, storage over the retention period — because \
                 stating them first is what separates a designed system from a \
                 remembered one. Then an ASCII component diagram showing the \
                 data flow. Then each component in a line. Then the two or \
                 three decisions that actually matter and what was traded away \
                 for each. Say which parts you would cut first under time \
                 pressure; knowing what is load-bearing is most of the \
                 skill.\n\nPrefer the boring mechanism teams actually ship — \
                 a unique constraint rather than a distributed lock. If the \
                 transcript shows they have already started, say what their \
                 answer misses before giving yours."
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
            Mode::Rehearsal.system_prompt(Depth::Full),
            Mode::Pairing.system_prompt(Depth::Full),
            Mode::Dev.system_prompt(Depth::Full),
        ];
        assert_ne!(prompts[0], prompts[1]);
        assert_ne!(prompts[1], prompts[2]);
    }

    /// The two depths are genuinely different tools, so a test keeps them so.
    #[test]
    fn depth_decides_whether_you_get_the_answer_or_a_nudge() {
        // Full depth gives you the artefact you are practising against.
        for mode in [Mode::Coding, Mode::Rehearsal] {
            let prompt = mode.system_prompt(Depth::Full).to_lowercase();
            assert!(
                prompt.contains("compiling code"),
                "{mode:?} at full depth should give code"
            );
        }
        assert!(
            Mode::SystemDesign
                .system_prompt(Depth::Full)
                .to_lowercase()
                .contains("ascii component diagram")
        );

        // A hint is a nudge, and says so.
        for mode in [Mode::Coding, Mode::SystemDesign] {
            let prompt = mode.system_prompt(Depth::Hint).to_lowercase();
            assert!(prompt.contains("nudge"), "{mode:?} hint should be a nudge");
            assert!(
                prompt.contains("under forty words") || prompt.contains("under sixty words"),
                "{mode:?} hint should cap its length"
            );
        }
    }
}
