//! Is the user at the Mac? Used to decide between quiet updates (menu bar and
//! the "现在" bar) and phone pushes, and to start the day's brief.
//!
//! On macOS the HID idle time comes from `ioreg`. A file `<data-dir>/presence`
//! containing `desk` or `away` overrides detection (used by tests).

use std::path::Path;

/// Idle longer than this means the user has left the Mac.
pub const AWAY_AFTER_SECS: f64 = 5.0 * 60.0;

/// Seconds since the last keyboard or mouse input, if known.
pub fn idle_seconds(data_dir: &Path) -> Option<f64> {
    if let Ok(s) = std::fs::read_to_string(data_dir.join("presence")) {
        match s.trim() {
            "desk" => return Some(0.0),
            "away" => return Some(f64::MAX),
            _ => {}
        }
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = std::process::Command::new("ioreg").args(["-c", "IOHIDSystem", "-d", "4"]).output().ok()?;
    parse_hid_idle(&String::from_utf8_lossy(&out.stdout))
}

fn parse_hid_idle(ioreg: &str) -> Option<f64> {
    let line = ioreg.lines().find(|l| l.contains("\"HIDIdleTime\""))?;
    let ns: f64 = line.rsplit('=').next()?.trim().parse().ok()?;
    Some(ns / 1e9)
}

/// Unknown presence counts as away, so the phone still gets told.
pub fn at_desk(data_dir: &Path) -> bool {
    idle_seconds(data_dir).map(|s| s < AWAY_AFTER_SECS).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ioreg() {
        let s = "    | |   \"HIDIdleTime\" = 2500000000\n";
        assert_eq!(parse_hid_idle(s), Some(2.5));
        assert_eq!(parse_hid_idle("nothing"), None);
    }

    #[test]
    fn override_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("presence"), "desk\n").unwrap();
        assert!(at_desk(d.path()));
        std::fs::write(d.path().join("presence"), "away").unwrap();
        assert!(!at_desk(d.path()));
    }
}
