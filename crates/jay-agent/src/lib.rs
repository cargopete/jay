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
pub mod echo;
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

impl Depth {
    pub const ALL: [Depth; 2] = [Depth::Full, Depth::Hint];

    pub fn label(self) -> &'static str {
        match self {
            Depth::Full => "answer",
            Depth::Hint => "nudge",
        }
    }
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
    /// Defending an answer you have already given.
    ///
    /// The half of an interview that follows the solution: "what's the time
    /// complexity", "why two layers", "what breaks at ten times the traffic".
    /// You are not building anything any more, you are being questioned about
    /// something already on the screen, and the reply has to be a sentence you
    /// can say out loud.
    ///
    /// It exists because [`Coding`](Mode::Coding) answers a follow-up question
    /// with a fresh implementation and [`SystemDesign`](Mode::SystemDesign)
    /// answers one with another capacity table and another diagram. Both are
    /// the right shape for the question that opens a round and the wrong shape
    /// for the twenty minutes after it. Observed, in a real loop: asked to walk
    /// through the view path, jay returned the whole design again, diagram
    /// included.
    Qa,
    /// Live pairing or coaching: concrete, short, opinionated.
    Pairing,
    /// Proactive dev assistant: reacting to a red test or a stack trace.
    Dev,
}

impl Mode {
    /// Every mode, in the order a mock loop runs them.
    pub const ALL: [Mode; 6] = [
        Mode::Coding,
        Mode::SystemDesign,
        Mode::Qa,
        Mode::Rehearsal,
        Mode::Pairing,
        Mode::Dev,
    ];

    /// Short enough for a switch on a small panel.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Coding => "code",
            Mode::SystemDesign => "design",
            Mode::Qa => "q&a",
            Mode::Rehearsal => "debrief",
            Mode::Pairing => "pair",
            Mode::Dev => "dev",
        }
    }

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
                 a partner playing interviewer, and is mid-interview. Give the \
                 solution in exactly this shape and nothing else:\n\n\
                 1. The approach, in one sentence they can say aloud.\n\
                 2. The code: complete, idiomatic, compiling — Rust unless \
                 they say otherwise — with the invariant that makes it correct \
                 named in one comment.\n\
                 3. One line: time and space complexity.\n\
                 4. At most three edge cases, one line each.\n\n\
                 The code must already handle every edge case it lists. Do not \
                 hand over a function with a known panic in it and mention the \
                 panic underneath — an interviewer reading that sees someone \
                 who spotted the bug and shipped it anyway.\n\n\
                 Hard limit: under 120 words of prose in total, outside the \
                 code. No preamble, no alternative implementations, no asides \
                 about what the interviewer might prefer, no closing summary. \
                 They are reading this while talking, and every sentence past \
                 the fourth costs them the thread. If the transcript shows \
                 they have already started, say what their approach gets \
                 wrong in one line before giving yours."
            }
            Mode::SystemDesign => {
                "The person is practising system design questions with a \
                 partner playing interviewer, and is mid-interview. Give the \
                 answer in exactly this shape and nothing else:\n\n\
                 1. The numbers that drive the design — request rates, read to \
                 write ratio, storage over the retention period — because \
                 stating them first is what separates a designed system from a \
                 remembered one.\n\
                 2. The component diagram, as a Mermaid `flowchart TD` in a \
                 ```mermaid fenced block. Mermaid because they redraw it in \
                 Excalidraw, which imports Mermaid directly, and because ASCII \
                 boxes are unreadable at panel width. Label every edge with what \
                 flows along it. Keep it to at most eight nodes.\n\
                 Write it for that importer, which is stricter than Mermaid's \
                 own renderer, both verified by hand: use only plain `[ ]` \
                 rectangles, never `[( )]` cylinders or `{ }` diamonds, because \
                 Excalidraw silently flattens them to rectangles anyway. Keep \
                 every node label to about three words — a long one is shrunk \
                 to unreadable while its neighbours stay full size, so \
                 `P[Postgres primary]` and not \
                 `P[(Postgres primary: metadata + bytea)]`. The detail belongs \
                 in part 3, where it can be read.\n\
                 3. Each component in one line.\n\
                 4. The two or three decisions that actually matter, one line \
                 each, saying what was traded away.\n\n\
                 **Size the design to the numbers you just stated, and no \
                 further.** The commonest failure in this mode is a diagram that \
                 would take a team a year: a floating IP, a load balancer, a \
                 separate cache tier, a queue and three datastores, for traffic a \
                 single box would serve. Every component must be justified by a \
                 number in part 1 or by a stated requirement. If a simpler design \
                 meets the numbers, draw the simpler design and name what would \
                 force the bigger one. An interviewer asks 'why is that there' \
                 about every box, and a box you added out of habit is the one \
                 that sinks the round.\n\n\
                 Hard limit: under 200 words of prose in total, outside the \
                 diagram. No preamble, no closing summary. Prefer the boring \
                 mechanism teams actually ship — a unique constraint rather \
                 than a distributed lock. If the transcript shows they have \
                 already started, say what their answer misses in one line \
                 before giving yours."
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
            Mode::Qa => {
                "The person has already given their answer — code on screen, or \
                 a diagram they drew — and is now being questioned about it. \
                 Every question here is 'why', 'what happens when', or 'walk me \
                 through'. They have to say your reply out loud, in their own \
                 voice, within a few seconds of reading it.\n\n\
                 So: plain prose. Short paragraphs, or at most three bullets. \
                 **No code. No diagrams. No capacity tables. No headings.** Not \
                 a smaller version of the original answer — an answer to the \
                 question that was actually asked, and nothing else. If they ask \
                 the time complexity, say the complexity and why; do not \
                 re-derive the solution around it.\n\n\
                 Hard limit: 120 words. Answer the question in the first \
                 sentence, then justify it. If the answer is a number, lead with \
                 the number.\n\n\
                 What is on screen is what they are defending. Refer to it — \
                 'your visited grid', 'the Varnish box' — rather than describing \
                 something you would have built. If their answer has a real \
                 problem the question is circling, say so in one line at the \
                 end, prefixed 'Watch out:'. Otherwise do not volunteer \
                 improvements; they are being examined, not redesigned."
            }
            Mode::Pairing => {
                "You are the second engineer in a pairing session. Be concrete, \
                 be brief, and have an opinion. Say the useful thing, not the \
                 complete thing.\n\n\
                 Hard limit: under 100 words of prose outside any code. No \
                 preamble, no summary, no survey of alternatives."
            }
            Mode::Dev => {
                "You are watching a developer work. Something just went wrong. \
                 Say what is most likely responsible and what to check first. \
                 Be specific and brief. Say plainly when you cannot tell.\n\n\
                 Hard limit: under 80 words, and at most two things to check, \
                 ordered so the first one distinguishes between the causes. \
                 This is read while a test is red and the person is already \
                 mid-thought; a second cause they will not get to is a second \
                 cause not worth generating."
            }
        }
    }
}

/// Appended to every mode's prompt. See [`Mode::system_prompt`] for why.
pub const LATE_ARRIVAL: &str = " You are always reading a conversation already \
    in progress: by the time this reaches them they will have started \
    answering. Read what they have already said and do not repeat it back. \
    Give only what is missing, wrong, or worth adding. If they have covered it \
    well, say so in one line and stop.\n\nYou have no tools, no filesystem and \
    no way to run anything, which is deliberate. Do not attempt to compile, \
    test or verify, and do not remark on being unable to — no notes about \
    sandboxes, tooling or what you would have checked. The person is \
    mid-conversation and every such sentence costs them the thread. Start with \
    the first substantive word: no preamble about your process.";

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

    /// The panel draws one switch per entry in `ALL` and sends the mode back.
    /// If a mode were ever added without being listed, it would simply be
    /// unreachable from the panel, silently.
    /// jay disables every tool, so the model has nothing to run. Left to
    /// Claude Code's own preamble it narrates that absence — one debrief
    /// opened "the sandbox here declined to run `rustc`", which is noise in
    /// front of an answer somebody is reading mid-interview.
    #[test]
    fn every_mode_is_told_it_has_no_tools() {
        for mode in Mode::ALL {
            for depth in Depth::ALL {
                let whole = format!("{}{}", mode.system_prompt(depth), LATE_ARRIVAL);
                assert!(
                    whole.contains("no tools"),
                    "{mode:?}/{depth:?} may narrate its own tooling"
                );
            }
        }
    }

    #[test]
    fn every_mode_is_reachable_from_the_switch_bank() {
        for mode in [
            Mode::Coding,
            Mode::SystemDesign,
            Mode::Rehearsal,
            Mode::Pairing,
            Mode::Dev,
        ] {
            assert!(Mode::ALL.contains(&mode), "{mode:?} is not on the panel");
        }
        let mut labels: Vec<_> = Mode::ALL.iter().map(|m| m.label()).collect();
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count, "two switches carry the same label");
    }

    /// Throwing a switch spawns a new process, so the pair must actually
    /// change what that process is told. If two positions produced the same
    /// system prompt, the switch would cost 4.7 seconds and buy nothing.
    #[test]
    fn each_switch_position_asks_for_something_different() {
        let mut seen = std::collections::HashSet::new();
        for mode in Mode::ALL {
            for depth in Depth::ALL {
                seen.insert(mode.system_prompt(depth));
            }
        }
        // Pairing and Dev share the coding hint, deliberately: a nudge is a
        // nudge. Everything else must be distinct.
        assert!(
            seen.len() >= 7,
            "only {} distinct prompts across {} switch positions",
            seen.len(),
            Mode::ALL.len() * Depth::ALL.len()
        );
    }

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
                prompt.contains("compiling") && prompt.contains("code"),
                "{mode:?} at full depth should give code"
            );
        }

        // The two mid-interview modes must cap their length, or they write an
        // essay: uncapped, "reverse a linked list" came back as 77 seconds of
        // alternatives and asides. Rehearsal is deliberately exempt — it runs
        // after the interview, where being thorough costs nobody the thread.
        for mode in [Mode::Coding, Mode::SystemDesign] {
            assert!(
                mode.system_prompt(Depth::Full)
                    .to_lowercase()
                    .contains("hard limit"),
                "{mode:?} is read mid-interview and must cap its length"
            );
        }
        // Mermaid rather than ASCII since a real round: box drawings are
        // unreadable at panel width, and Excalidraw imports Mermaid directly,
        // so the diagram can be redrawn rather than retyped.
        assert!(
            Mode::SystemDesign
                .system_prompt(Depth::Full)
                .to_lowercase()
                .contains("mermaid")
        );

        // The mode that exists because the other two answer a follow-up
        // question by giving the whole solution again.
        let qa = Mode::Qa.system_prompt(Depth::Full).to_lowercase();
        for forbidden in ["no code", "no diagrams", "hard limit"] {
            assert!(qa.contains(forbidden), "q&a must say {forbidden:?}");
        }

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
