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

pub mod claude;
pub mod gate;
pub mod screen;

/// What jay is being used for. Changes what it is asked, not what it can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// You are in a live interview, right now, already talking.
    ///
    /// The tightest mode by a distance. Everything that makes a good written
    /// engineering answer — code, structure, completeness — is useless here,
    /// because you cannot read a fenced SQL block aloud to an interviewer who
    /// is waiting for you to finish a sentence.
    Interview,
    /// Mock interview practice: talking points and an outline to think with,
    /// plus what the answer you actually gave left out.
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
            Mode::Interview => {
                "The person you are helping is in a live technical interview \
                 right now and is already speaking. Everything you write must \
                 be sayable out loud, immediately, with no translation. Plain \
                 spoken sentences only: no code, no markdown, no headings, no \
                 bullet lists, no numbered options, no formatting of any kind. \
                 At most three points, most valuable first, under sixty words \
                 in total. Prefer one excellent point to three adequate ones. \
                 If they have already covered it well, reply with the single \
                 word: covered. Give the boring solution that teams actually \
                 ship, not the sophisticated one: a unique constraint rather \
                 than a distributed lock, a cache rather than a custom \
                 protocol. Interviewers mark people down for reaching past the \
                 problem, and a candidate who names the simple mechanism and \
                 says why it is enough sounds more senior than one who reaches \
                 for the impressive answer."
            }
            Mode::Rehearsal => {
                "You are helping someone rehearse for an interview. Give them \
                 points to think with, never a paragraph to read aloud. An \
                 answer they assemble themselves survives a follow-up \
                 question; one they recite does not. Be brief."
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

    #[test]
    fn rehearsal_asks_for_points_rather_than_a_script() {
        let prompt = Mode::Rehearsal.system_prompt().to_lowercase();
        assert!(prompt.contains("never a paragraph to read aloud"));
    }
}
