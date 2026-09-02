//! Meeting notes from a session transcript.
//!
//! The other half of a transcriber. A log of everything said is the record;
//! nobody reads a forty-minute one twice. This turns it into the page you
//! actually keep: what was decided, who owes what, and what nobody answered.
//!
//! Two things separate these notes from the ones every meeting tool generates,
//! and both come from the transcript rather than from the prompt.
//!
//! **Every claim carries the time it came from.** jay stamps each line from
//! when the speech began, so a decision can point at `[12:04]` and the reader
//! can go and listen to it. A summary you cannot check against the recording
//! is a summary you have to take on faith.
//!
//! **Attribution is physical.** `you` and `them` are two separate audio
//! channels — a microphone and a system tap — not one stream a diarizer has
//! guessed at. An action item assigned to the wrong person is worse than no
//! action item, and this is the one part of the problem jay does not have to
//! guess at.
//!
//! Like everything else jay asks a model for, this goes through the `claude`
//! CLI on the subscription. Unlike everything else, it does not want to be
//! Claude Code while it does it — see [`shed_the_agent`].

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The model these notes are written by.
///
/// Sonnet rather than Opus deliberately. This is summarisation over a document
/// that is entirely present in the prompt, which is the task tier Sonnet is
/// for; the reasoning tier is for the live suggestions, where the answer is
/// not in the transcript.
pub const DEFAULT_MODEL: &str = "claude-sonnet-5";

/// How long to wait before giving up.
///
/// Generous, unlike the 45 seconds a live suggestion gets. Nothing is waiting
/// on this: the meeting is over, the transcript is already on disk, and a page
/// that takes two minutes is a page that arrives before anybody looks for it.
const TIMEOUT: Duration = Duration::from_secs(300);

/// Tools the CLI must be told, by name, that it does not have.
///
/// `--allowed-tools ""` is not enough, and the difference is not small. It
/// stops the tools being *called*; it leaves their definitions in the prompt.
/// Measured on this machine, one `claude --print` call carrying a one-word
/// question:
///
/// ```text
/// default system prompt, --allowed-tools ""   42,535 prompt tokens
/// own system prompt,     --allowed-tools ""   33,911
/// own system prompt,     these disallowed     13,609
/// ```
///
/// Two thirds of the prompt, for a task that involves no tools at all. The
/// tokens are the smaller half of it: what is actually being removed is twenty
/// thousand tokens of instruction about being a coding agent, sitting in front
/// of a request to summarise a meeting.
///
/// A name that no longer exists is harmless — the CLI ignores it — so this
/// list going stale costs nothing but the tokens it stops removing. Worth
/// re-measuring after a CLI upgrade.
const SHED: &str = "Bash,Read,Write,Edit,Glob,Grep,WebFetch,WebSearch,Task,\
TodoWrite,NotebookEdit,BashOutput,KillShell,SlashCommand,Skill,Agent,\
ExitPlanMode,AskUserQuestion";

#[derive(Debug, thiserror::Error)]
pub enum NotesError {
    #[error("the transcript has nothing in it to summarise")]
    Empty,
    #[error("running the claude CLI: {0}")]
    Spawn(String),
    #[error("claude: {0}")]
    Cli(String),
    #[error("the reply had no notes in it")]
    NoText,
}

type Result<T> = std::result::Result<T, NotesError>;

/// What one run of the notes cost, and how long it took.
///
/// `usd` is the CLI's own `total_cost_usd`. On a subscription that is an
/// imputed list price rather than money leaving an account, which is the same
/// figure the rest of jay reports and the same caveat applies to all of it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Spent {
    pub prompt_tokens: u64,
    pub output_tokens: u64,
    pub usd: f64,
    pub elapsed: Duration,
}

/// A finished set of notes.
#[derive(Debug, Clone)]
pub struct Notes {
    /// Markdown, ready to be written next to the session.
    pub markdown: String,
    pub spent: Spent,
    pub model: String,
}

/// How the notes are told to read.
///
/// Short, and every rule in it is a rule about honesty rather than about
/// style. The failure mode of a meeting summary is not that it reads badly,
/// it is that it confidently asserts a decision nobody made.
const SYSTEM: &str = "\
You write the notes for a meeting, from its transcript.

The transcript is a jay session archive. Read its header. Every line is stamped \
`[mm:ss]` or `[mm:ss–mm:ss]` from when the speech began, `you:` is the person \
running the recording and `them:` is everyone else on the call, and a `?` \
before a speaker means the transcriber heard the line but did not trust it.

Parenthesised lines are the recorder talking about itself, not somebody \
speaking. Two of them change how you read the lines around them:

- `(the earlier \"you\" copy of \"...\" was this room, not you; dropped from \
context)` means the `you:` line just above is the *same sentence* as a `them:` \
line at the same timestamp — the other person's voice arriving through the \
microphone as well as the call. Ignore the `you:` copy entirely. It is not the \
recorder speaking, and treating it as one is how an action ends up assigned to \
the wrong person.
- `(kept the line above out of context — ...)` gives the reason a `?` line was \
distrusted.

Rules, in order of how much they matter.

1. Cite the time. Every decision, action and open question ends with the \
timestamp it came from, in square brackets, exactly as the transcript writes \
it. A reader must be able to go back and listen to the moment.
2. Do not invent. If something was discussed and left unresolved, it is an open \
question, not a decision. If nobody committed to doing something, it is not an \
action. An empty section is a true section; write `- none` under it.
3. Say when you are unsure. If a point rests on a `?` line, or on a passage \
that reads as garbled, say so in the line itself. The transcriber mishears \
names and jargon constantly.
4. Attribute by channel, never by guessing. `you` and `them` come from two \
separate microphones and are reliable. Which *particular* person on the far \
side spoke is not knowable — say `them` rather than picking a name, unless \
somebody was addressed by name in the audio.
5. Be brief. Someone reads this instead of the transcript; a page they skim is \
a page that failed.

Reply with markdown only, no preamble, in exactly this shape:

# <a short title naming what the meeting was about>

<two or three sentences: what this was, and what came out of it.>

## Decisions
- **<what was decided>** — <one line on why, or what it rules out> [mm:ss]

## Actions
- **you** — <what, specifically> [mm:ss]
- **them** — <what, specifically> [mm:ss]

## Open questions
- <the question, as asked> [mm:ss]

## Thread
- **<topic>** [mm:ss–mm:ss] — <up to two lines on what was said>
";

/// Strip Claude Code back to a model that summarises meetings.
///
/// Two flags, and both matter. `--system-prompt` *replaces* Claude Code's own
/// system prompt rather than appending to it, which is what the rest of jay
/// does; `--disallowed-tools` removes the tool definitions, which
/// `--allowed-tools ""` does not. See [`SHED`] for what that is worth.
///
/// The result is a model that has been told one thing — how to write meeting
/// notes — and nothing about being an agent in a repository.
fn shed_the_agent(command: &mut Command, model: &str, system: &str) {
    command
        .arg("--print")
        .arg("--model")
        .arg(model)
        .arg("--output-format")
        .arg("json")
        .arg("--system-prompt")
        .arg(system)
        .arg("--disallowed-tools")
        .arg(SHED)
        // Belt and braces: nothing may be called even if a name above has
        // been renamed out from under this list.
        .arg("--allowed-tools")
        .arg("")
        // Summarising a document entirely present in the prompt does not need
        // the top of the range, and effort is most of the output bill.
        .arg("--effort")
        .arg("medium");
}

/// Write the notes for one session transcript.
///
/// `transcript` is the archive file's contents, header and all: the header
/// explains the stamp format and the `?` convention, so it is cheaper to send
/// it than to restate it.
pub fn write(transcript: &str, model: &str, roster: Option<&str>) -> Result<Notes> {
    // A transcript of nothing produces a page of confident nothing, which is
    // the single most expensive way to be wrong here.
    if !has_speech(transcript) {
        return Err(NotesError::Empty);
    }

    let started = Instant::now();
    let binary = std::env::var("JAY_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
    let mut command = Command::new(&binary);
    // Run from a neutral directory. `claude --print` inherits the working
    // directory and will happily reach for whatever repository it is standing
    // in; point jay at a client's codebase and that becomes a leak.
    command.current_dir(std::env::temp_dir());

    // The roster is appended rather than woven in, so the standing instructions
    // are byte-identical from session to session and stay cacheable.
    //
    // It is the only thing that can put a name on the far side. jay's channel
    // separation is physical — one microphone each way — so six people on a
    // call are six people called `them`, and no amount of prompting recovers
    // from that on its own. Given who was in the room, a name can be *inferred*
    // where the transcript makes it plain: somebody introduces themselves,
    // somebody is handed the floor by name, somebody is answered by name.
    // Everywhere else it stays `them`, and rule 4 above is unchanged for a
    // reason — a confidently wrong name is worse than an honest `them`.
    let system = match roster {
        Some(names) => format!(
            "{SYSTEM}\n\nExpected on this call: {names}.\nUse these names for the \
             far side ONLY where the transcript makes it plain who was speaking — \
             they introduce themselves, they are handed the floor by name, or they \
             are answered by name. Everywhere else keep `them`. A name you inferred \
             from topic or manner is a guess, and a guess here is worse than `them`."
        ),
        None => SYSTEM.to_string(),
    };
    shed_the_agent(&mut command, model, &system);

    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| NotesError::Spawn(e.to_string()))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| NotesError::Spawn("no stdin on the claude process".into()))?;
        // On stdin rather than as an argument: a long meeting is a large
        // document, and argv has a ceiling that a transcript can reach.
        write!(stdin, "Here is the session transcript.\n\n{transcript}")
            .map_err(|e| NotesError::Spawn(e.to_string()))?;
        // Dropped here: the CLI waits for end-of-input before it will answer,
        // so holding this open hangs the whole call.
    }

    // Drained on its own thread. Left unread, a full stderr pipe blocks the
    // child mid-answer, which looks exactly like a slow model.
    let errors = child.stderr.take().map(|stderr| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                buf.push_str(&line);
                buf.push('\n');
            }
            buf
        })
    });

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| NotesError::Spawn("no stdout on the claude process".into()))?;

    let mut markdown = None;
    let mut spent = Spent::default();
    let mut reported = None;

    for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
        // The CLI prints the odd non-JSON line; not fatal, and not the result.
        let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        let Some(text) = event.get("result").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if event["is_error"].as_bool().unwrap_or(false) {
            reported = Some(text.to_string());
            continue;
        }
        markdown = Some(text.trim().to_string());
        spent.usd = event["total_cost_usd"].as_f64().unwrap_or(0.0);
        let usage = &event["usage"];
        // Three fields, because the split between them depends on whether the
        // CLI's prefix happened to be warm and none of them alone is "how big
        // was the prompt".
        spent.prompt_tokens = usage["input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
        spent.output_tokens = usage["output_tokens"].as_u64().unwrap_or(0);
    }

    let status = child.wait().map_err(|e| NotesError::Spawn(e.to_string()))?;
    let stderr = errors
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    if let Some(message) = reported {
        return Err(NotesError::Cli(message));
    }
    if !status.success() {
        return Err(NotesError::Cli(match stderr.trim() {
            "" => format!("exited {status} with nothing on stderr"),
            complaint => complaint.to_string(),
        }));
    }

    spent.elapsed = started.elapsed();
    if spent.elapsed > TIMEOUT {
        tracing::warn!(seconds = spent.elapsed.as_secs_f32(), "the notes took a while");
    }

    match markdown {
        Some(markdown) if !markdown.is_empty() => Ok(Notes {
            markdown,
            spent,
            model: model.to_string(),
        }),
        _ => Err(NotesError::NoText),
    }
}

/// Did anybody actually say anything in this archive?
///
/// A session that heard nothing still produces a file: the header, and a
/// notice naming the devices. Handed that, a summariser writes a plausible
/// page about a meeting that did not happen, which is worse than an error.
fn has_speech(transcript: &str) -> bool {
    transcript.lines().any(is_speech)
}

/// `[02:00–02:21] them: …`, or the same with a `?`. Not a notice, which is
/// parenthesised, and not the header.
fn is_speech(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('[') else {
        return false;
    };
    let Some((_stamp, said)) = rest.split_once("] ") else {
        return false;
    };
    let said = said.strip_prefix('?').unwrap_or(said);
    said.starts_with("you: ") || said.starts_with("them: ")
}

/// Where the notes for a session live: beside it, and obviously derived.
pub fn path_for(session: &std::path::Path) -> std::path::PathBuf {
    let stem = session
        .file_stem()
        .map_or_else(|| "session".to_string(), |s| s.to_string_lossy().to_string());
    session.with_file_name(format!("{stem}.notes.md"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "# jay session\n\nTimes are minutes:seconds.\n\n";

    /// Checked before the CLI is ever spawned, so this costs nothing and,
    /// more to the point, produces no page about a meeting that did not
    /// happen.
    #[test]
    fn a_session_that_heard_nothing_is_not_summarised() {
        let silent = format!(
            "{HEADER}[00:00] (listening on you: MacBook Pro Microphone, them: AirPods.)\n"
        );
        assert!(!has_speech(&silent));
        assert!(matches!(
            write(&silent, DEFAULT_MODEL, None).unwrap_err(),
            NotesError::Empty
        ));
    }

    #[test]
    fn one_spoken_line_is_enough() {
        let heard = format!("{HEADER}[00:21–00:33] them: the problem is a rate limiter\n");
        assert!(has_speech(&heard));
    }

    /// A line jay did not trust is still a line somebody said.
    #[test]
    fn an_unsure_line_counts_as_speech() {
        assert!(is_speech("[01:04–01:06] ?you: token bucket, per tenant"));
    }

    #[test]
    fn notices_and_suggestions_are_not_speech() {
        assert!(!is_speech("[00:00] (listening on you: the default input device)"));
        assert!(!is_speech("[04:00] --- jay ---"));
        assert!(!is_speech("Times are minutes:seconds from the first audio."));
        assert!(!is_speech(""));
    }

    /// A missing `claude` is a missing `claude`, not a mystery.
    #[test]
    fn a_cli_that_is_not_there_says_so() {
        let heard = format!("{HEADER}[00:21–00:33] them: the problem is a rate limiter\n");
        // SAFETY: single-threaded test, and the value is restored below.
        unsafe { std::env::set_var("JAY_CLAUDE_BIN", "jay-no-such-binary") };
        let err = write(&heard, DEFAULT_MODEL, None).unwrap_err();
        unsafe { std::env::remove_var("JAY_CLAUDE_BIN") };
        assert!(matches!(err, NotesError::Spawn(_)), "{err}");
    }

    /// The list is what makes the prompt two thirds smaller. An empty or
    /// whitespace-bearing entry would be silently ignored by the CLI, so the
    /// saving would quietly stop happening.
    #[test]
    fn every_shed_tool_is_a_bare_name() {
        let names: Vec<&str> = SHED.split(',').collect();
        assert!(names.len() > 10, "the list looks truncated: {SHED}");
        for name in names {
            assert_eq!(name, name.trim(), "{name:?} carries whitespace");
            assert!(!name.is_empty());
            assert!(
                name.chars().all(char::is_alphanumeric),
                "{name:?} is not a plain tool name"
            );
        }
    }

    #[test]
    fn notes_sit_beside_the_session_they_came_from() {
        let session = std::path::Path::new("/tmp/sessions/2026-08-27-1400.md");
        assert_eq!(
            path_for(session),
            std::path::Path::new("/tmp/sessions/2026-08-27-1400.notes.md")
        );
    }


}
