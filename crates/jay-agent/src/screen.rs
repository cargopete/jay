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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Target {
    /// The whole main display. The default, for three reasons.
    ///
    /// It needs only Screen Recording, where the per-window path also needs
    /// Accessibility. It cannot pick the wrong window — and it very nearly
    /// always would, because clicking jay's ask button focuses jay, so the
    /// "frontmost window" at capture time is the panel rather than the code.
    /// And in a design discussion the arrangement is often the point: the
    /// diagram beside the code beside the terminal.
    #[default]
    Display,
    /// A single window, by id.
    ///
    /// Tighter and cheaper in tokens, and fragile: identifying the window
    /// means asking System Events, which needs Accessibility permission and
    /// which some applications simply refuse — `alacritty` returns "can't get
    /// id of window 1". Use it when you know it works for your setup.
    FocusedWindow,
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

/// Window ID of the frontmost window that is not jay's own panel.
///
/// The panel is why this is not simply "the frontmost window". Pressing the
/// ask button focuses the panel, so by the time this runs the frontmost window
/// *is* jay — and the screenshot sent to the model would be a picture of the
/// transcript rather than of the code being discussed. Silently useless, and
/// exactly the sort of thing nobody notices until an answer is inexplicably
/// vague.
///
/// CoreGraphics would avoid the subprocess, but `CGWindowListCopyWindowInfo`
/// needs the same permission and rather more FFI for a value used once per
/// suggestion.
fn frontmost_window_id() -> Result<u32> {
    let script = r#"tell application "System Events"
        set candidates to every application process whose visible is true
        repeat with p in candidates
            if name of p is not "jay" then
                if (count of windows of p) > 0 then
                    if frontmost of p is true then
                        return id of first window of p
                    end if
                end if
            end if
        end repeat
        -- Nothing else is frontmost, which means jay has focus. Fall back to
        -- the most recently active other application with a window.
        repeat with p in candidates
            if name of p is not "jay" then
                if (count of windows of p) > 0 then
                    return id of first window of p
                end if
            end if
        end repeat
        error "no window to capture"
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
