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

/// Apps where the user reads Codex output: terminals, editors with a terminal, the Codex app.
const CODEX_FRONT_APPS: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "com.mitchellh.ghostty",
    "net.kovidgoyal.kitty",
    "io.alacritty",
    "com.github.wez.wezterm",
    "com.microsoft.VSCode",
    "com.todesktop.230313mzl4w4u92", // Cursor
    "com.openai.codex",
    "com.openai.chat",
];

/// Idle shorter than this while a Codex window is in front means the user is watching it.
pub const WATCHING_SECS: f64 = 120.0;

/// The bundle id of the frontmost app (macOS), or the `front` override file's content in tests.
pub fn front_app(data_dir: &Path) -> Option<String> {
    if let Ok(s) = std::fs::read_to_string(data_dir.join("front")) {
        return Some(s.trim().to_string());
    }
    if !cfg!(target_os = "macos") {
        return None;
    }
    let front = std::process::Command::new("lsappinfo").arg("front").output().ok()?;
    let asn = String::from_utf8_lossy(&front.stdout).trim().to_string();
    if asn.is_empty() {
        return None;
    }
    let info = std::process::Command::new("lsappinfo").args(["info", "-only", "bundleid", &asn]).output().ok()?;
    parse_bundle_id(&String::from_utf8_lossy(&info.stdout))
}

fn parse_bundle_id(s: &str) -> Option<String> {
    // "CFBundleIdentifier"="com.apple.Terminal"
    let v = s.rsplit('=').next()?.trim().trim_matches('"').to_string();
    (!v.is_empty() && v != "[ NULL ]").then_some(v)
}

/// The user is at the Mac and looking at a terminal or Codex window right now. We can't tell which
/// session (one terminal may hold several), so this is "watching Codex" in general.
pub fn watching_codex(data_dir: &Path) -> bool {
    let recent = idle_seconds(data_dir).is_some_and(|s| s < WATCHING_SECS);
    recent && front_app(data_dir).is_some_and(|b| CODEX_FRONT_APPS.contains(&b.as_str()))
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
    fn parses_bundle_id() {
        assert_eq!(parse_bundle_id("\"CFBundleIdentifier\"=\"com.apple.Terminal\"\n").as_deref(), Some("com.apple.Terminal"));
        assert_eq!(parse_bundle_id(""), None);
    }

    #[test]
    fn watching_needs_recent_input_and_a_codex_window() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("presence"), "desk").unwrap();
        std::fs::write(d.path().join("front"), "com.apple.Terminal").unwrap();
        assert!(watching_codex(d.path()));
        std::fs::write(d.path().join("front"), "com.apple.Safari").unwrap();
        assert!(!watching_codex(d.path()));
        std::fs::write(d.path().join("presence"), "away").unwrap();
        std::fs::write(d.path().join("front"), "com.apple.Terminal").unwrap();
        assert!(!watching_codex(d.path()));
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
