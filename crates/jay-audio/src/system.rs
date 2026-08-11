//! System audio capture on macOS.
//!
//! This is the "what the machine is playing" side: the other people on a call,
//! a recorded talk, whatever is coming out of the speakers. It is a capture
//! path, not a use case, and it is what makes jay useful on an incident bridge
//! or in a pairing session rather than only when you talk to yourself.
//!
//! macOS gives two routes and both need an FFI shim:
//!
//! - **CoreAudio process taps** (`AudioHardwareCreateProcessTap`, macOS 14.4+).
//!   Audio only, can tap a specific process, no screen recording involved.
//!   This machine runs 26.5.1 so the API is available.
//! - **ScreenCaptureKit** (`SCStream` with audio enabled, macOS 12.3+). Older,
//!   more widely copied, but it drags in the screen-recording permission even
//!   when only audio is wanted.
//!
//! Process taps are the better fit and are what this module will use. Both
//! require user consent, which is the point: jay asks, it does not sneak.
//!
//! Not yet implemented. Tracked as its own piece of work.

use crossbeam_channel::Sender;

use crate::{AudioError, Frame, Result};

/// Start capturing system output audio.
///
/// Frames arrive on `tx` tagged [`crate::Channel::System`].
pub fn start(_tx: Sender<Frame>) -> Result<()> {
    Err(AudioError::NoDevice("system audio (not yet implemented)"))
}
