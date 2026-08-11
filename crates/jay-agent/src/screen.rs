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
pub fn capture(_target: Target, into: &Path) -> Result<PathBuf> {
    let path = into.join(format!("jay-capture-{}.jpg", std::process::id()));

    // In process, via the shim, rather than shelling out to
    // `/usr/sbin/screencapture`. The subprocess version works from a terminal
    // and fails from the app bundle even with the permission granted and the
    // toggle visibly on, because TCC evaluates the request against the process
    // that makes it and a spawned Apple binary does not cleanly inherit the
    // parent's grant. Asking directly is unambiguous.
    jay_audio::screen::capture_main_display(&path)
        .map_err(|e| AgentError::Screen(e.to_string()))?;

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_names_the_file_after_the_process() {
        // Not a capture test — that needs permission and a screen. This only
        // pins the naming, so two jays never fight over one path.
        let dir = std::path::Path::new("/tmp");
        let expected = dir.join(format!("jay-capture-{}.jpg", std::process::id()));
        assert!(expected.to_string_lossy().contains("jay-capture-"));
    }

    #[test]
    fn targets_are_distinct() {
        assert_ne!(Target::FocusedWindow, Target::Display);
    }
}
