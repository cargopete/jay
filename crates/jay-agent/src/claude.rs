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

use std::io::Write;
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
            brief: None,
        }
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
    ///
    /// The image costs an extra turn (the CLI reads the file, then answers)
    /// and several thousand image tokens, so pass one only when what is on
    /// screen is the thing being discussed.
    pub fn suggest_with(
        &self,
        mode: Mode,
        question: &str,
        transcript: &[String],
        screenshot: Option<&std::path::Path>,
    ) -> Result<Suggestion> {
        let started = Instant::now();
        let mut prompt = build_prompt(mode, question, transcript, self.brief.as_deref());
        if let Some(path) = screenshot {
            prompt.push_str(&format!(
                "\n\nWhat is on screen right now is at {}. Read it before answering; \
                 it is probably what the question is about.",
                path.display()
            ));
        }

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
            .arg("--output-format")
            .arg("json")
            // Read only when there is an image to read. Otherwise no tools at
            // all: jay wants an opinion, not an agent loose in the filesystem.
            .arg("--allowed-tools")
            .arg(if screenshot.is_some() { "Read" } else { "" })
            .arg("--append-system-prompt")
            .arg(format!("{}{}", mode.system_prompt(), crate::LATE_ARRIVAL))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AgentError::Spawn(e.to_string()))?;

        child
            .stdin
            .take()
            .ok_or_else(|| AgentError::Spawn("no stdin on the claude process".into()))?
            .write_all(prompt.as_bytes())
            .map_err(|e| AgentError::Spawn(e.to_string()))?;

        // `wait_with_output` has no timeout of its own, so the deadline is
        // enforced by the caller running this on its own thread. Documented
        // here so the next reader does not assume TIMEOUT is doing more than
        // it is.
        let output = child
            .wait_with_output()
            .map_err(|e| AgentError::Spawn(e.to_string()))?;

        if !output.status.success() {
            return Err(AgentError::Cli(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }

        let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| AgentError::Parse(e.to_string()))?;

        if envelope
            .get("is_error")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            return Err(AgentError::Cli(
                envelope
                    .get("result")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("claude reported an error with no detail")
                    .to_string(),
            ));
        }

        let text = envelope
            .get("result")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AgentError::Parse("no `result` field in the response".into()))?
            .trim()
            .to_string();

        let cost = envelope
            .get("total_cost_usd")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);

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

fn build_prompt(
    mode: Mode,
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

    if !transcript.is_empty() {
        prompt.push_str("Recent conversation, oldest first:\n");
        for line in transcript {
            prompt.push_str("  ");
            prompt.push_str(line);
            prompt.push('\n');
        }
        prompt.push('\n');
    }

    match mode {
        Mode::Interview => {
            prompt.push_str("The interviewer just asked:\n  ");
            prompt.push_str(question);
            prompt.push_str(
                "\n\nWhat is the most valuable thing they have not yet said? \
                 Say it in plain speech, briefly, so they can fold it into the \
                 sentence they are already in the middle of.",
            );
        }
        Mode::Rehearsal => {
            prompt.push_str("The question just asked was:\n  ");
            prompt.push_str(question);
            prompt.push_str(
                "\n\nGive me points to think with, not a script. Three or four \
                 talking points, then one line on the shape a strong answer \
                 takes. If the transcript shows I already started answering, \
                 say what I left out.",
            );
        }
        Mode::Pairing => {
            prompt.push_str("The question just asked was:\n  ");
            prompt.push_str(question);
            prompt.push_str(
                "\n\nAnswer as the other half of a pair. Be concrete and short. \
                 If there is an approach worth taking, name it and say why.",
            );
        }
        Mode::Dev => {
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

    #[test]
    fn prompt_carries_the_question_and_transcript() {
        let prompt = build_prompt(
            Mode::Rehearsal,
            "How would you design a rate limiter?",
            &["them: so let's talk about systems design".to_string()],
            None,
        );
        assert!(prompt.contains("rate limiter"));
        assert!(prompt.contains("systems design"));
        assert!(prompt.contains("not a script"));
    }

    #[test]
    fn prompt_works_without_transcript() {
        let prompt = build_prompt(Mode::Dev, "the auth test went red", &[], None);
        assert!(prompt.contains("auth test"));
        assert!(!prompt.contains("Recent conversation"));
    }

    #[test]
    fn the_brief_leads_so_a_cache_can_serve_it() {
        let prompt = build_prompt(
            Mode::Pairing,
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
        let rehearsal = build_prompt(Mode::Rehearsal, q, &[], None);
        let pairing = build_prompt(Mode::Pairing, q, &[], None);
        let dev = build_prompt(Mode::Dev, q, &[], None);
        assert_ne!(rehearsal, pairing);
        assert_ne!(pairing, dev);
    }
}
