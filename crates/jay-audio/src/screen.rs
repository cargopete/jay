//! Screen capture, in process.
//!
//! It lives beside the audio tap because it shares the shim and, more to the
//! point, the same lesson: on macOS a capability is granted to a *process*,
//! and anything that muddies which process is asking will be refused in a way
//! that looks like a bug in your code.
//!
//! jay originally shelled out to `/usr/sbin/screencapture`. That works from a
//! terminal, and fails from the app bundle with "could not create image from
//! display" — with Screen Recording granted, the toggle visibly on, and the
//! same flags succeeding from a shell one second earlier. A spawned Apple
//! binary does not cleanly inherit the parent's grant. Asking directly does.

use std::ffi::CString;
use std::path::Path;

use crate::{AudioError, Result};

unsafe extern "C" {
    fn jay_capture_main_display(path: *const std::ffi::c_char) -> i32;
}

const NO_IMAGE: i32 = -1;
const WRITE_FAILED: i32 = -2;

/// Capture the main display to `path` as a PNG.
pub fn capture_main_display(path: &Path) -> Result<()> {
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|_| AudioError::SystemTap("path contains a nul byte".into()))?;

    // SAFETY: the shim reads the string for the duration of the call and
    // writes only to the file it names.
    match unsafe { jay_capture_main_display(c_path.as_ptr()) } {
        0 => Ok(()),
        NO_IMAGE => Err(AudioError::SystemTap(
            "the display returned no image, which is what Screen Recording \
             permission looks like when it has been declined"
                .into(),
        )),
        WRITE_FAILED => Err(AudioError::SystemTap(format!(
            "could not write {}",
            path.display()
        ))),
        other => Err(AudioError::SystemTap(format!(
            "screen capture failed with {other}"
        ))),
    }
}
