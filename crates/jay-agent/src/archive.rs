//! Where a session goes when it is over.
//!
//! Every run leaves a file, without being asked to. That is the whole feedback
//! loop: the most valuable input this project has had was a recording of a
//! real interview, which produced the small-talk filter, the late-arrival
//! prompts, and most of the test corpus. A loop that depends on remembering a
//! flag is a loop that does not run.
//!
//! The file holds the conversation *and* what jay said, with elapsed times and
//! what each suggestion cost. That makes it an evaluation set rather than a
//! log: any moment in it can be replayed through the real prompt path with
//! `jay ask --context`, which is the only honest way to tell whether a change
//! helped.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Directory holding every session.
pub fn sessions_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("JAY_SESSION_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home {
        Some(home) if cfg!(target_os = "macos") => home
            .join("Library")
            .join("Application Support")
            .join("jay")
            .join("sessions"),
        Some(home) => home.join(".local").join("share").join("jay").join("sessions"),
        None => PathBuf::from("jay-sessions"),
    }
}

/// A path for a session starting now.
///
/// Named by UTC timestamp rather than anything cleverer: it sorts correctly,
/// it never collides, and it needs no dependency to produce.
pub fn new_session_path() -> PathBuf {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    sessions_dir().join(format!("{}.md", stamp(secs)))
}

/// `YYYY-MM-DD-HHMMSS` from a Unix timestamp, in UTC.
///
/// Hand-rolled from the civil-from-days algorithm rather than pulling in a
/// date crate for one format string in a program that has no other need of
/// calendars.
pub fn stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let time = secs % 86_400;
    let (hour, minute, second) = (time / 3600, (time % 3600) / 60, time % 60);

    // Howard Hinnant's civil_from_days, shifted to an era beginning 0000-03-01.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}-{hour:02}{minute:02}{second:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_a_known_instant() {
        // 2021-01-01T00:00:00Z
        assert_eq!(stamp(1_609_459_200), "2021-01-01-000000");
        // 2026-08-15T12:00:00Z — mid-era, well past the last leap day.
        assert_eq!(stamp(1_786_795_200), "2026-08-15-120000");
        // 2024-02-29T23:59:59Z — the case a hand-rolled calendar gets wrong.
        assert_eq!(stamp(1_709_251_199), "2024-02-29-235959");
        // The epoch itself.
        assert_eq!(stamp(0), "1970-01-01-000000");
    }

    #[test]
    fn stamps_sort_chronologically_as_strings() {
        // The whole reason for this format.
        let earlier = stamp(1_609_459_200);
        let later = stamp(1_786_795_200);
        assert!(earlier < later);
    }

    #[test]
    fn the_session_dir_can_be_overridden() {
        // SAFETY: single-threaded test, read on the next line.
        unsafe { std::env::set_var("JAY_SESSION_DIR", "/tmp/jay-sessions-test") };
        assert_eq!(sessions_dir(), PathBuf::from("/tmp/jay-sessions-test"));
        unsafe { std::env::remove_var("JAY_SESSION_DIR") };
    }
}
