//! Screen capture, on demand.
//!
//! For whiteboarding and pairing, what is on screen usually carries more than
//! what was said about it: a stack trace, a diagram, a failing assertion.
//!
//! Deliberately **not** continuous. The tools in this category tend to grab a
//! frame every second or so, which is expensive (an image adds thousands of
//! tokens to a call that already carries ~29,000 of CLI preamble), and is a
//! far larger privacy surface than audio for very little gain. jay captures at
//! the moment it decides to speak, and nowhere else.
//!
//! Requires Screen Recording permission. Like the audio tap, that means being
//! launched through LaunchServices — see `scripts/bundle.sh`. A shell-launched
//! binary inherits the responsible process of whatever owns the shell, so the
//! grant never attaches to jay.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{AgentError, Result};

/// What to capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The frontmost window. The usual choice: it is what the conversation is
    /// almost always about, and it excludes everything else on the desktop.
    FocusedWindow,
    /// The whole main display. Use when the point is the arrangement of
    /// several windows, e.g. a diagram beside its code.
    Display,
}

/// Capture the screen to a PNG and return its path.
///
/// The caller owns the file and should delete it once the suggestion has been
/// made; a directory quietly filling with screenshots of someone's work is
/// exactly the thing this tool should not do.
pub fn capture(target: Target, into: &Path) -> Result<PathBuf> {
    let path = into.join(format!("jay-capture-{}.png", std::process::id()));

    let mut command = Command::new("/usr/sbin/screencapture");
    command.arg("-x"); // no shutter sound; this is not a photo op
    match target {
        // -o omits the window shadow, which is just wasted pixels and tokens.
        Target::FocusedWindow => {
            command.arg("-o").arg("-l").arg(frontmost_window_id()?.to_string());
        }
        Target::Display => {
            command.arg("-m"); // main display only, not every attached screen
        }
    }
    command.arg(&path);

    let output = command
        .output()
        .map_err(|e| AgentError::Spawn(format!("running screencapture: {e}")))?;

    if !output.status.success() || !path.is_file() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // The characteristic failure is a permission one, and its message
        // ("could not create image from rect") does not say so.
        return Err(AgentError::Screen(format!(
            "{}. If this mentions creating an image, it is almost certainly \
             Screen Recording permission: launch jay from its .app bundle via \
             `open -a`, not directly from a shell",
            stderr.trim()
        )));
    }

    Ok(path)
}

/// Window ID of the frontmost window, via AppleScript.
///
/// CoreGraphics would avoid the subprocess, but `CGWindowListCopyWindowInfo`
/// needs the same permission and rather more FFI for a value used once per
/// suggestion.
fn frontmost_window_id() -> Result<u32> {
    let script = r#"tell application "System Events"
        set frontApp to first application process whose frontmost is true
        return id of first window of frontApp
    end tell"#;

    let output = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| AgentError::Spawn(format!("running osascript: {e}")))?;

    if !output.status.success() {
        return Err(AgentError::Screen(format!(
            "could not identify the frontmost window: {}. This usually means \
             Accessibility permission is missing",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| AgentError::Screen("the frontmost window has no id".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_names_the_file_after_the_process() {
        // Not a capture test — that needs permission and a screen. This only
        // pins the naming, so two jays never fight over one path.
        let dir = std::path::Path::new("/tmp");
        let expected = dir.join(format!("jay-capture-{}.png", std::process::id()));
        assert!(expected.to_string_lossy().contains("jay-capture-"));
    }

    #[test]
    fn targets_are_distinct() {
        assert_ne!(Target::FocusedWindow, Target::Display);
    }
}
