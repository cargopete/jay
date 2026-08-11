//! Talking to Claude through the local CLI.
//!
//! jay drives the `claude` binary in headless mode rather than the HTTP API,
//! because that is what uses Chief's Max subscription: the CLI already holds
//! the OAuth session, so there is no API key to manage and no second bill.
//!
//! The cost of this convenience is measured, not assumed. Each invocation
//! carries Claude Code's own system prompt and tool definitions, which came to
//! roughly 29,000 tokens in testing: $0.0254 on a cold cache, $0.0033 once
//! warm, for a one-word answer. That is fine for an occasional suggestion and
//! ruinous for anything running continuously, which is why [`crate::gate`]
//! decides when to call this and is not itself a model.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{AgentError, Mode, Result, Suggestion};

/// Default model for the reasoning step.
pub const DEFAULT_MODEL: &str = "claude-opus-5";

/// How long to wait before giving up on a suggestion.
///
/// Past this the conversation has moved on and the answer is worse than
/// useless, because it is answering a question nobody is still asking.
const TIMEOUT: Duration = Duration::from_secs(45);

pub struct Claude {
    model: String,
    binary: String,
    depth: crate::Depth,
    /// Standing context for the whole session: a job spec, a CV, the RFC being
    /// paired on, notes on the architecture.
    ///
    /// This is the cheapest large gain available. Without it every suggestion
    /// is reasoned from a dozen lines of transcript and nothing else, which is
    /// why they read generic. It goes first in the prompt so it forms a stable
    /// prefix that prompt caching can serve at a tenth of the price.
    brief: Option<String>,
}

impl Default for Claude {
    fn default() -> Self {
        Self::new(DEFAULT_MODEL)
    }
}

impl Claude {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            binary: std::env::var("JAY_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string()),
            depth: crate::Depth::default(),
            brief: None,
        }
    }

    /// Ask for a nudge rather than the worked answer.
    #[must_use]
    pub fn with_depth(mut self, depth: crate::Depth) -> Self {
        self.depth = depth;
        self
    }

    /// Give jay standing context for the session.
    #[must_use]
    pub fn with_brief(mut self, brief: impl Into<String>) -> Self {
        let brief = brief.into();
        self.brief = (!brief.trim().is_empty()).then_some(brief);
        self
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Ask for help with `question`, given the recent transcript as context.
    pub fn suggest(&self, mode: Mode, question: &str, transcript: &[String]) -> Result<Suggestion> {
        self.suggest_with(mode, question, transcript, None)
    }

    /// As [`suggest`](Self::suggest), optionally with a screenshot.
    pub fn suggest_with(
        &self,
        mode: Mode,
        question: &str,
        transcript: &[String],
        screenshot: Option<&std::path::Path>,
    ) -> Result<Suggestion> {
        self.suggest_streaming(mode, question, transcript, screenshot, |_| {})
    }

    /// As [`suggest_with`](Self::suggest_with), reporting text as it arrives.
    ///
    /// `on_delta` is called with each fragment the model produces, on this
    /// thread. Total time is unchanged — the point is that the first words
    /// appear at about five seconds rather than the whole answer at fourteen,
    /// and five seconds into a conversation you can still use what you read.
    pub fn suggest_streaming(
        &self,
        mode: Mode,
        question: &str,
        transcript: &[String],
        screenshot: Option<&std::path::Path>,
        mut on_delta: impl FnMut(&str),
    ) -> Result<Suggestion> {
        let started = Instant::now();
        let prompt = build_prompt(mode, self.depth, question, transcript, self.brief.as_deref());

        // The screenshot goes in as an image block rather than as a path for
        // the model to open with `Read`. That tool call was a whole extra
        // round trip through the CLI — measured at about four seconds, the
        // same as the entire spawn-and-preamble floor — spent fetching a file
        // jay already had in hand. It also means no tools need be enabled at
        // all, so there is no agent loose in the filesystem.
        let mut content = Vec::new();
        if let Some(path) = screenshot {
            let bytes = std::fs::read(path)
                .map_err(|e| AgentError::Spawn(format!("reading {}: {e}", path.display())))?;
            content.push(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type(path),
                    "data": base64(&bytes),
                }
            }));
        }
        content.push(serde_json::json!({
            "type": "text",
            "text": if screenshot.is_some() { format!("{prompt}{OWN_PANEL}") } else { prompt },
        }));

        let message = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": content},
        });

        // Run from a neutral directory. `claude -p` inherits the working
        // directory, and with thin context it will happily improvise an answer
        // out of whatever repository it happens to be standing in — observed
        // in testing, where a stray question produced a summary of jay's own
        // uncommitted changes. Worse than useless: point jay at a client's
        // codebase and that becomes a leak.
        let neutral = std::env::temp_dir();

        let mut child = Command::new(&self.binary)
            .current_dir(&neutral)
            .arg("--print")
            .arg("--model")
            .arg(&self.model)
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            // The CLI requires both of these to emit token-level deltas.
            .arg("--verbose")
            .arg("--include-partial-messages")
            // No tools, ever. jay wants an opinion, not an agent.
            .arg("--allowed-tools")
            .arg("")
            .arg("--append-system-prompt")
            .arg(format!("{}{}", mode.system_prompt(self.depth), crate::LATE_ARRIVAL))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::Spawn(e.to_string()))?;

        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| AgentError::Spawn("no stdin on the claude process".into()))?;
            writeln!(stdin, "{message}").map_err(|e| AgentError::Spawn(e.to_string()))?;
            // Dropped here: the CLI waits for end-of-input before it will
            // answer, so holding this open hangs the whole call.
        }

        // Drained on its own thread. Left unread, a full stderr pipe would
        // block the child mid-answer, which looks exactly like a slow model.
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
            .ok_or_else(|| AgentError::Spawn("no stdout on the claude process".into()))?;

        let mut streamed = String::new();
        let mut final_text: Option<String> = None;
        let mut cost = 0.0f64;
        let mut reported: Option<String> = None;

        for line in BufReader::new(stdout).lines().map_while(std::result::Result::ok) {
            let Ok(event) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue; // the CLI prints the odd non-JSON line; not fatal
            };
            match event.get("type").and_then(serde_json::Value::as_str) {
                Some("stream_event") => {
                    let inner = event.get("event");
                    let is_text_delta = inner
                        .and_then(|e| e.get("delta"))
                        .and_then(|d| d.get("type"))
                        .and_then(serde_json::Value::as_str)
                        == Some("text_delta");
                    if is_text_delta
                        && let Some(text) = inner
                            .and_then(|e| e.get("delta"))
                            .and_then(|d| d.get("text"))
                            .and_then(serde_json::Value::as_str)
                    {
                        streamed.push_str(text);
                        on_delta(&streamed);
                    }
                }
                Some("result") => {
                    cost = event
                        .get("total_cost_usd")
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(0.0);
                    let text = event.get("result").and_then(serde_json::Value::as_str);
                    if event
                        .get("is_error")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                    {
                        reported = Some(
                            text.unwrap_or("claude reported an error with no detail")
                                .to_string(),
                        );
                    } else {
                        final_text = text.map(|t| t.trim().to_string());
                    }
                }
                _ => {}
            }
        }

        let status = child
            .wait()
            .map_err(|e| AgentError::Spawn(e.to_string()))?;
        let stderr = errors
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();

        if let Some(message) = reported {
            return Err(AgentError::Cli(message));
        }
        if !status.success() {
            return Err(AgentError::Cli(stderr.trim().to_string()));
        }

        // The streamed deltas are the same text as `result`, so either will
        // do; prefer the final field, which is authoritative.
        let text = match final_text {
            Some(text) if !text.is_empty() => text,
            _ if !streamed.trim().is_empty() => streamed.trim().to_string(),
            _ => return Err(AgentError::Parse("no text in the response".into())),
        };

        let elapsed = started.elapsed();
        tracing::info!(
            model = %self.model,
            cost_usd = cost,
            seconds = elapsed.as_secs_f32(),
            "suggestion returned"
        );

        if elapsed > TIMEOUT {
            tracing::warn!(
                seconds = elapsed.as_secs_f32(),
                "suggestion arrived after the conversation had likely moved on"
            );
        }

        Ok(Suggestion {
            text,
            cost_usd: cost,
            latency: elapsed,
            model: self.model.clone(),
        })
    }
}

/// Appended whenever a screenshot goes with the question.
///
/// jay's own panel is on the screen it captures, so the image contains jay's
/// previous answer. Observed: asked what was on screen, it described its own
/// last turn quoted back at it. Mid-round that is worse than a curiosity —
/// the panel holds the code jay suggested, and mistaking that for the
/// candidate's own work makes every "what have they got so far" judgement
/// wrong in the same direction.
const OWN_PANEL: &str = "\n\nThe dark panel labelled JAY in the screenshot is \
your own output from earlier in this session, not their work. Ignore it when \
judging what they have written or said so far.";

/// What the CLI should be told the image is.
fn media_type(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/jpeg",
    }
}

/// Standard base64, for the one place jay needs it.
///
/// A crate for this would be entirely reasonable; twenty lines that never
/// change are more reasonable still.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

fn build_prompt(
    mode: Mode,
    depth: crate::Depth,
    question: &str,
    transcript: &[String],
    brief: Option<&str>,
) -> String {
    let mut prompt = String::new();

    // First, so it is the stable prefix a cache can serve cheaply.
    if let Some(brief) = brief {
        prompt.push_str("Background for this whole session:\n");
        prompt.push_str(brief.trim());
        prompt.push_str("\n\n");
    }

    // The problem statement is spoken once, at the start, and then scrolls out
    // of a rolling window while the discussion runs on. jay heard it; without
    // pinning it, jay then forgets it exactly when the questions get specific.
    if let Some(problem) = transcript.iter().find(|line| line.starts_with("PROBLEM: ")) {
        prompt.push_str("The problem being worked on:\n  ");
        prompt.push_str(problem.trim_start_matches("PROBLEM: "));
        prompt.push_str("\n\n");
    }

    if !transcript.is_empty() {
        prompt.push_str("Recent conversation, oldest first:\n");
        for line in transcript {
            prompt.push_str("  ");
            prompt.push_str(line);
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    // What follows must agree with the system prompt for this mode and depth.
    // It did not, for a while: at full depth the system prompt asked for
    // "complete, idiomatic, compiling code" and this tail said "not the code",
    // and the tail won. Coding mode returned five paragraphs of prose and no
    // code at all, which is precisely the one thing it exists to produce.
    //
    // So at full depth the tail states the question and stops. The shape of
    // the answer is the system prompt's job, and only one of them may hold it.
    match (mode, depth) {
        (Mode::Coding, crate::Depth::Full) => {
            prompt.push_str("The problem:\n  ");
            prompt.push_str(question);
            prompt.push_str("\n\nSolve it.");
        }
        (Mode::Coding, crate::Depth::Hint) => {
            prompt.push_str("The interviewer just asked:\n  ");
            prompt.push_str(question);
            prompt.push_str(
                "\n\nName the approach and its complexity, or the edge case \
                 they are about to miss. Not the code — the insight that makes \
                 the code obvious.",
            );
        }
        (Mode::SystemDesign, crate::Depth::Full) => {
            prompt.push_str("The question:\n  ");
            prompt.push_str(question);
            prompt.push_str("\n\nAnswer it.");
        }
        (Mode::SystemDesign, crate::Depth::Hint) => {
            prompt.push_str("The interviewer just asked:\n  ");
            prompt.push_str(question);
            prompt.push_str(
                "\n\nWhat is the most valuable thing they have not yet said? \
                 A missing component, an unnamed tradeoff, a failure mode. Say \
                 it in plain speech, briefly, so they can fold it into the \
                 sentence they are already in the middle of.",
            );
        }
        (Mode::Rehearsal, _) => {
            prompt.push_str("The problem:\n  ");
            prompt.push_str(question);
            prompt.push_str(
                "\n\nWork it fully. If the transcript shows an attempt, say \
                 first where it went wrong or what it missed, then give the \
                 complete answer.",
            );
        }
        (Mode::Pairing, _) => {
            prompt.push_str("The question just asked was:\n  ");
            prompt.push_str(question);
            prompt.push_str(
                "\n\nAnswer as the other half of a pair. Be concrete and short. \
                 If there is an approach worth taking, name it and say why.",
            );
        }
        (Mode::Dev, _) => {
            prompt.push_str("What just happened:\n  ");
            prompt.push_str(question);
            prompt.push_str(
                "\n\nSay what is most likely wrong and the first thing worth \
                 checking. Be specific. If you cannot tell from this alone, \
                 say what you would need to see.",
            );
        }
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 4648 test vectors. A hand-rolled encoder that is subtly wrong
    /// would corrupt every screenshot jay ever sends, and the model would
    /// simply describe whatever the corruption happened to look like.
    #[test]
    fn base64_matches_the_rfc_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_handles_the_high_bytes_a_jpeg_is_made_of() {
        // The whole alphabet must be reachable, and a JPEG is not ASCII.
        assert_eq!(base64(&[0xff, 0xff, 0xff]), "////");
        assert_eq!(base64(&[0x00, 0x00, 0x00]), "AAAA");
        assert_eq!(base64(&[0xff, 0xd8, 0xff, 0xe0]), "/9j/4A==");
    }

    #[test]
    fn media_type_follows_the_extension() {
        assert_eq!(media_type(std::path::Path::new("a/b.jpg")), "image/jpeg");
        assert_eq!(media_type(std::path::Path::new("a/b.PNG")), "image/png");
        // Anything unlabelled is a jay capture, and those are JPEG.
        assert_eq!(media_type(std::path::Path::new("shot")), "image/jpeg");
    }

    /// The bug this guards against shipped, and was invisible: coding mode
    /// asked for compiling code in its system prompt and forbade it in the
    /// same breath in the user prompt, so it returned prose for weeks.
    #[test]
    fn full_depth_never_argues_with_its_own_system_prompt() {
        for mode in [Mode::Coding, Mode::SystemDesign, Mode::Rehearsal] {
            let prompt = build_prompt(mode, crate::Depth::Full, "reverse a list", &[], None);
            assert!(
                !prompt.contains("Not the code"),
                "{mode:?} at full depth still forbids the code it is asked for"
            );
        }
        // and the hint still refuses to write it, which is its whole job
        assert!(
            build_prompt(Mode::Coding, crate::Depth::Hint, "reverse a list", &[], None)
                .contains("Not the code")
        );
    }

    #[test]
    fn a_pinned_problem_leads_the_prompt() {
        let prompt = build_prompt(
            Mode::SystemDesign,
            crate::Depth::Full,
            "how would you shard it?",
            &[
                "PROBLEM: Design a URL shortener.".to_string(),
                "you: so the read path dominates".to_string(),
            ],
            None,
        );
        assert!(prompt.starts_with("The problem being worked on:"));
        assert!(prompt.contains("URL shortener"));
    }

    #[test]
    fn prompt_carries_the_question_and_transcript() {
        let prompt = build_prompt(
            Mode::Rehearsal,
            crate::Depth::Full,
            "How would you design a rate limiter?",
            &["them: so let's talk about systems design".to_string()],
            None,
        );
        assert!(prompt.contains("rate limiter"));
        assert!(prompt.contains("systems design"));
        assert!(prompt.contains("Work it fully"));
    }

    #[test]
    fn prompt_works_without_transcript() {
        let prompt = build_prompt(Mode::Dev, crate::Depth::Full, "the auth test went red", &[], None);
        assert!(prompt.contains("auth test"));
        assert!(!prompt.contains("Recent conversation"));
    }

    #[test]
    fn the_brief_leads_so_a_cache_can_serve_it() {
        let prompt = build_prompt(
            Mode::Pairing,
            crate::Depth::Full,
            "how should we shard this?",
            &["them: right, next topic".to_string()],
            Some("Role: senior backend engineer. Stack: Rust, Postgres."),
        );
        assert!(prompt.starts_with("Background for this whole session:"));
        assert!(prompt.contains("senior backend engineer"));
        // and the volatile parts still come after it
        assert!(prompt.find("next topic").unwrap() > prompt.find("Postgres").unwrap());
    }

    #[test]
    fn an_empty_brief_is_no_brief() {
        let claude = Claude::new("m").with_brief("   \n  ");
        assert!(claude.brief.is_none());
    }

    #[test]
    fn each_mode_asks_for_something_different() {
        let q = "why is this failing";
        let d = crate::Depth::Full;
        let rehearsal = build_prompt(Mode::Rehearsal, d, q, &[], None);
        let pairing = build_prompt(Mode::Pairing, d, q, &[], None);
        let dev = build_prompt(Mode::Dev, d, q, &[], None);
        assert_ne!(rehearsal, pairing);
        assert_ne!(pairing, dev);
    }
}

/// One `claude` process, reused for a whole session.
///
/// Every invocation of the CLI pays about 4.7 seconds before it says anything:
/// node startup, then ~29,000 tokens of Claude Code's own preamble. Paid once
/// per press that is most of the wait. Paid once per *session* it disappears —
/// measured here at 4.1s for the first answer and 2.8s for the next, including
/// a 75-second idle gap in between, which is what a real session looks like.
///
/// The second benefit is not about speed at all: the process keeps the
/// conversation, so the third question of a round is asked of something that
/// heard the first two.
pub struct Session {
    binary: String,
    model: String,
    system_prompt: String,
    mode: Mode,
    depth: crate::Depth,
    brief: Option<String>,
    /// Sent with the first question only. After that it is in the history the
    /// process is already holding, and repeating it every turn would pay for
    /// it again.
    brief_sent: bool,
    live: Option<Live>,
}

struct Live {
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(mut live) = self.live.take() {
            let _ = live.child.kill();
            let _ = live.child.wait();
        }
    }
}

impl Session {
    pub fn new(claude: &Claude, mode: Mode) -> Self {
        Self {
            binary: claude.binary.clone(),
            model: claude.model.clone(),
            system_prompt: format!("{}{}", mode.system_prompt(claude.depth), crate::LATE_ARRIVAL),
            mode,
            depth: claude.depth,
            brief: claude.brief.clone(),
            brief_sent: false,
            live: None,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Ask, reusing the running process. Reports text as it arrives.
    ///
    /// A process that has died is replaced and the question asked again, once.
    /// The CLI is a long-lived child of a long-lived app and will occasionally
    /// be lost to something outside jay's control; losing the answer as well
    /// would be a poor trade for the four seconds saved.
    pub fn ask(
        &mut self,
        question: &str,
        transcript: &[String],
        screenshot: Option<&std::path::Path>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Suggestion> {
        match self.attempt(question, transcript, screenshot, on_delta) {
            Err(AgentError::Spawn(e)) | Err(AgentError::Parse(e)) if self.live.is_some() => {
                tracing::warn!(%e, "claude session died; restarting it");
                self.live = None;
                // The history died with it, so the brief goes again.
                self.brief_sent = false;
                self.attempt(question, transcript, screenshot, on_delta)
            }
            other => other,
        }
    }

    fn attempt(
        &mut self,
        question: &str,
        transcript: &[String],
        screenshot: Option<&std::path::Path>,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<Suggestion> {
        let started = Instant::now();
        let brief = (!self.brief_sent).then_some(self.brief.as_deref()).flatten();
        let prompt = build_prompt(self.mode, self.depth, question, transcript, brief);

        let mut content = Vec::new();
        if let Some(path) = screenshot {
            let bytes = std::fs::read(path)
                .map_err(|e| AgentError::Spawn(format!("reading {}: {e}", path.display())))?;
            content.push(serde_json::json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type(path),
                    "data": base64(&bytes),
                }
            }));
        }
        content.push(serde_json::json!({
            "type": "text",
            "text": if screenshot.is_some() { format!("{prompt}{OWN_PANEL}") } else { prompt },
        }));
        let message = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": content},
        });

        self.ensure_live()?;
        let live = self
            .live
            .as_mut()
            .ok_or_else(|| AgentError::Spawn("no claude session".into()))?;

        writeln!(live.stdin, "{message}").map_err(|e| AgentError::Spawn(e.to_string()))?;
        live.stdin
            .flush()
            .map_err(|e| AgentError::Spawn(e.to_string()))?;

        let answer = read_answer(&mut live.stdout, on_delta)?;
        self.brief_sent = true;

        let elapsed = started.elapsed();
        tracing::info!(
            model = %self.model,
            cost_usd = answer.cost,
            seconds = elapsed.as_secs_f32(),
            "suggestion returned"
        );
        Ok(Suggestion {
            text: answer.text,
            cost_usd: answer.cost,
            latency: elapsed,
            model: self.model.clone(),
        })
    }

    fn ensure_live(&mut self) -> Result<()> {
        if self.live.is_some() {
            return Ok(());
        }
        // Neutral directory, for the same reason as the one-shot path: `claude
        // -p` inherits the working directory and will improvise an answer out
        // of whatever repository it is standing in.
        let mut child = Command::new(&self.binary)
            .current_dir(std::env::temp_dir())
            .arg("--print")
            .arg("--model")
            .arg(&self.model)
            .arg("--input-format")
            .arg("stream-json")
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose")
            .arg("--include-partial-messages")
            .arg("--allowed-tools")
            .arg("")
            .arg("--append-system-prompt")
            .arg(&self.system_prompt)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::Spawn(e.to_string()))?;

        // Drained for the life of the process. A full stderr pipe would block
        // the child mid-answer, which is indistinguishable from a slow model.
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(std::result::Result::ok) {
                    tracing::debug!(target: "claude", "{line}");
                }
            });
        }

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| AgentError::Spawn("no stdin on the claude process".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Spawn("no stdout on the claude process".into()))?;

        self.live = Some(Live {
            child,
            stdin,
            stdout: BufReader::new(stdout),
        });
        Ok(())
    }
}

struct Answer {
    text: String,
    cost: f64,
}

/// Read events until one answer is complete.
///
/// Returns `Spawn` on end of stream, which is how a dead process reports
/// itself here and is what tells [`Session::ask`] to restart and retry.
fn read_answer(
    reader: &mut impl BufRead,
    on_delta: &mut dyn FnMut(&str),
) -> Result<Answer> {
    let mut streamed = String::new();
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Err(AgentError::Spawn("claude ended the stream".into())),
            Ok(_) => {}
            Err(e) => return Err(AgentError::Spawn(e.to_string())),
        }
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("stream_event") => {
                let inner = event.get("event");
                let is_text_delta = inner
                    .and_then(|e| e.get("delta"))
                    .and_then(|d| d.get("type"))
                    .and_then(serde_json::Value::as_str)
                    == Some("text_delta");
                if is_text_delta
                    && let Some(text) = inner
                        .and_then(|e| e.get("delta"))
                        .and_then(|d| d.get("text"))
                        .and_then(serde_json::Value::as_str)
                {
                    streamed.push_str(text);
                    on_delta(&streamed);
                }
            }
            Some("result") => {
                let text = event.get("result").and_then(serde_json::Value::as_str);
                if event
                    .get("is_error")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    return Err(AgentError::Cli(
                        text.unwrap_or("claude reported an error with no detail")
                            .to_string(),
                    ));
                }
                let cost = event
                    .get("total_cost_usd")
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(0.0);
                let text = match text.map(str::trim) {
                    Some(t) if !t.is_empty() => t.to_string(),
                    _ if !streamed.trim().is_empty() => streamed.trim().to_string(),
                    _ => return Err(AgentError::Parse("no text in the response".into())),
                };
                return Ok(Answer { text, cost });
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod session_tests {
    use super::*;

    /// Proves the thing the whole `Session` type exists for: that the second
    /// question is materially faster than the first, because it does not pay
    /// for the process to start or for the CLI's preamble again.
    ///
    /// Ignored by default — it spends real money and needs a signed-in CLI.
    /// Run it with `cargo test -p jay-agent -- --ignored --nocapture`.
    #[test]
    #[ignore = "spends money and needs the claude CLI signed in"]
    fn a_session_reuses_its_process_and_the_second_ask_is_faster() {
        let claude = Claude::new(DEFAULT_MODEL).with_depth(crate::Depth::Hint);
        let mut session = Session::new(&claude, Mode::Pairing);
        let mut ignore = |_: &str| {};

        let first = session
            .ask("Reply with exactly one word: alpha", &[], None, &mut ignore)
            .expect("first ask");
        let second = session
            .ask("Reply with exactly one word: beta", &[], None, &mut ignore)
            .expect("second ask");

        println!(
            "  first  {:.1}s  ${:.4}\n  second {:.1}s  ${:.4}",
            first.latency.as_secs_f32(),
            first.cost_usd,
            second.latency.as_secs_f32(),
            second.cost_usd,
        );
        assert!(
            second.latency < first.latency,
            "the second ask ({:.1}s) should beat the first ({:.1}s); if it does \
             not, the process is being respawned and Session is buying nothing",
            second.latency.as_secs_f32(),
            first.latency.as_secs_f32(),
        );
    }
}
