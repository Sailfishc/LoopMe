//! Codex hooks: how LoopMe sees the user's own Codex sessions.
//!
//! Codex (0.166+) runs command hooks listed in `$CODEX_HOME/hooks.json`
//! (default `~/.codex/hooks.json`). A hook only runs once its hash is marked
//! trusted in the user's `config.toml` (`hooks.state."<key>".trusted_hash`);
//! LoopMe does that through the app-server (`hooks/list` + `config/batchWrite`)
//! after the user's one-time confirm.
//!
//! The hook command is this binary: `loopme-daemon hook --loopme-data-dir DIR`.
//! It reads the event JSON on stdin, drops a small file into
//! `DIR/inbox/`, and exits. The daemon picks the files up (`sessions::ingest`).
//! For `PermissionRequest` it can wait for an answer from the phone, which the
//! daemon writes to `DIR/inbox/resp/<event id>.json`.

use crate::codex::CodexClient;
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const EVENTS: &[&str] = &["SessionStart", "UserPromptSubmit", "Stop", "PermissionRequest", "SessionEnd", "Interrupt"];
/// Marker that identifies LoopMe's entries in hooks.json.
pub const MARKER: &str = "hook --loopme-data-dir";
/// Environment variable set on LoopMe's own Codex process so its hooks are skipped.
pub const INTERNAL_ENV: &str = "LOOPME_INTERNAL";
/// How long a permission request may wait for the phone (the hook timeout is a bit longer).
pub const APPROVAL_WAIT_SECS: u64 = 14 * 60;
const APPROVAL_HOOK_TIMEOUT: u64 = 15 * 60;

pub fn inbox_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("inbox")
}

pub fn resp_path(data_dir: &Path, event_id: &str) -> PathBuf {
    inbox_dir(data_dir).join("resp").join(format!("{event_id}.json"))
}

fn trunc(s: &str, n: usize) -> String {
    crate::notify::truncate(s, n)
}

/// The compact form written to the inbox.
pub fn compact_event(raw: &Value, id: &str, at_ms: i64) -> Value {
    let g = |k: &str| raw.get(k).and_then(Value::as_str).map(str::to_string);
    let tool_input = raw.get("tool_input").map(|v| match v {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("command")
            .map(|c| match c {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
                other => other.to_string(),
            })
            .unwrap_or_else(|| v.to_string()),
        other => other.to_string(),
    });
    json!({
        "id": id,
        "at": at_ms,
        "event": g("hook_event_name"),
        "sessionId": g("session_id"),
        "turnId": g("turn_id"),
        "cwd": g("cwd"),
        "transcriptPath": g("transcript_path"),
        "source": g("source"),
        "prompt": g("prompt").map(|s| trunc(&s, 600)),
        "lastMessage": g("last_assistant_message").map(|s| trunc(&s, 2000)),
        "toolName": g("tool_name"),
        "toolInput": tool_input.map(|s| trunc(&s, 600)),
    })
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Entry point of `loopme-daemon hook`. Never fails Codex: always exits 0.
pub fn hook_main(data_dir: &Path) {
    if std::env::var(INTERNAL_ENV).as_deref() == Ok("1") {
        return;
    }
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let Ok(raw) = serde_json::from_str::<Value>(&input) else { return };
    let id = format!("hk{}", uuid::Uuid::new_v4().simple());
    let at = crate::model::now_ms();
    let ev = compact_event(&raw, &id, at);
    let dir = inbox_dir(data_dir);
    if std::fs::create_dir_all(dir.join("resp")).is_err() {
        return;
    }
    if write_atomic(&dir.join(format!("{at}-{id}.json")), ev.to_string().as_bytes()).is_err() {
        return;
    }
    if ev["event"] == "PermissionRequest" {
        if let Some(out) = wait_for_answer(data_dir, &id) {
            println!("{out}");
        }
    }
}

/// Waits for the daemon's answer to a permission request. The daemon first
/// says whether the user is away (`{"wait":true}`); then the final
/// `{"behavior":"allow"|"deny"|null}`. No answer means Codex asks as usual.
fn wait_for_answer(data_dir: &Path, id: &str) -> Option<String> {
    let path = resp_path(data_dir, id);
    let start = Instant::now();
    let mut waiting = false;
    loop {
        let limit = if waiting { Duration::from_secs(APPROVAL_WAIT_SECS) } else { Duration::from_secs(4) };
        if start.elapsed() > limit {
            let _ = std::fs::remove_file(&path);
            return None;
        }
        if let Ok(s) = std::fs::read_to_string(&path) {
            if let Ok(v) = serde_json::from_str::<Value>(&s) {
                if let Some(b) = v.get("behavior") {
                    let _ = std::fs::remove_file(&path);
                    let b = b.as_str()?;
                    if b != "allow" && b != "deny" {
                        return None;
                    }
                    let mut decision = json!({"behavior": b});
                    if b == "deny" {
                        decision["message"] = json!("用户在手机上拒绝了");
                    }
                    return Some(
                        json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}}).to_string(),
                    );
                }
                if v.get("wait").and_then(Value::as_bool) == Some(false) {
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
                waiting = true;
            }
        }
        std::thread::sleep(Duration::from_millis(if waiting { 1000 } else { 100 }));
    }
}

// ---------------------------------------------------------------------------
// Installing and trusting

pub fn codex_home() -> PathBuf {
    if let Ok(h) = std::env::var("CODEX_HOME") {
        if !h.is_empty() {
            return PathBuf::from(h);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".codex")
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

pub fn hook_command(exe: &Path, data_dir: &Path) -> String {
    format!("{} {} {}", shell_quote(&exe.display().to_string()), MARKER, shell_quote(&data_dir.display().to_string()))
}

fn is_ours(handler: &Value) -> bool {
    handler.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(MARKER))
}

/// Returns `hooks.json` with LoopMe's entries removed (and added back when `command` is set).
/// Other hooks the user has are kept as they are.
pub fn rewrite_hooks_json(existing: Option<&str>, command: Option<&str>) -> Result<Value> {
    let mut root: Value = match existing.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => serde_json::from_str(s).context("hooks.json 不是合法的 JSON，未改动")?,
        None => json!({}),
    };
    if !root.is_object() {
        return Err(anyhow!("hooks.json 格式不认识，未改动"));
    }
    if !root.get("hooks").is_some_and(Value::is_object) {
        root["hooks"] = json!({});
    }
    let hooks = root["hooks"].as_object_mut().unwrap();
    for groups in hooks.values_mut() {
        if let Some(arr) = groups.as_array_mut() {
            for g in arr.iter_mut() {
                if let Some(hs) = g.get_mut("hooks").and_then(Value::as_array_mut) {
                    hs.retain(|h| !is_ours(h));
                }
            }
            arr.retain(|g| g.get("hooks").and_then(Value::as_array).is_none_or(|h| !h.is_empty()));
        }
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    if let Some(cmd) = command {
        for ev in EVENTS {
            let timeout = if *ev == "PermissionRequest" { APPROVAL_HOOK_TIMEOUT } else { 10 };
            let entry = json!({"hooks": [{"type": "command", "command": cmd, "timeout": timeout}]});
            match hooks.get_mut(*ev).and_then(Value::as_array_mut) {
                Some(arr) => arr.push(entry),
                None => {
                    hooks.insert(ev.to_string(), json!([entry]));
                }
            }
        }
    }
    Ok(root)
}

fn update_file(command: Option<&str>) -> Result<()> {
    let dir = codex_home();
    let path = dir.join("hooks.json");
    let existing = std::fs::read_to_string(&path).ok();
    let next = rewrite_hooks_json(existing.as_deref(), command)?;
    std::fs::create_dir_all(&dir)?;
    let body = serde_json::to_string_pretty(&next)? + "\n";
    if existing.as_deref() != Some(body.as_str()) {
        write_atomic(&path, body.as_bytes())?;
    }
    Ok(())
}

/// Adds LoopMe's hooks to hooks.json (idempotent).
pub fn install(exe: &Path, data_dir: &Path) -> Result<()> {
    update_file(Some(&hook_command(exe, data_dir)))
}

/// Removes LoopMe's hooks from hooks.json; the user's own hooks stay.
pub fn uninstall() -> Result<()> {
    update_file(None)
}

/// Lists LoopMe's hooks as Codex sees them: (key, current hash, trust status).
async fn our_hooks(client: &CodexClient) -> Result<Vec<(String, String, String)>> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let res = client.request("hooks/list", json!({"cwds": [home]})).await?;
    let mut out = vec![];
    for entry in res.get("data").and_then(Value::as_array).cloned().unwrap_or_default() {
        for h in entry.get("hooks").and_then(Value::as_array).cloned().unwrap_or_default() {
            if !h.get("command").and_then(Value::as_str).is_some_and(|c| c.contains(MARKER)) {
                continue;
            }
            let key = h.get("key").and_then(Value::as_str).unwrap_or_default().to_string();
            let hash = h.get("currentHash").and_then(Value::as_str).unwrap_or_default().to_string();
            let trust = h.get("trustStatus").and_then(Value::as_str).unwrap_or_default().to_string();
            if !key.is_empty() && !out.iter().any(|(k, _, _)| k == &key) {
                out.push((key, hash, trust));
            }
        }
    }
    Ok(out)
}

/// Marks LoopMe's hooks as trusted in the user's Codex config. Returns how many are active.
pub async fn trust(client: &CodexClient) -> Result<usize> {
    let hooks = our_hooks(client).await?;
    let edits: Vec<Value> = hooks
        .iter()
        .filter(|(_, _, t)| t != "trusted" && t != "managed")
        .map(|(k, h, _)| {
            json!({
                "keyPath": format!("hooks.state.{}.trusted_hash", serde_json::to_string(k).unwrap_or_default()),
                "value": h,
                "mergeStrategy": "upsert"
            })
        })
        .collect();
    if !edits.is_empty() {
        client.request("config/batchWrite", json!({"edits": edits})).await?;
    }
    Ok(hooks.len())
}

/// `on` when every LoopMe hook is installed and trusted, `untrusted` when some
/// are not (e.g. the app moved), `off` when none are installed.
pub async fn status(client: &CodexClient) -> Result<String> {
    let hooks = our_hooks(client).await?;
    if hooks.is_empty() {
        return Ok("off".into());
    }
    if hooks.len() >= EVENTS.len() && hooks.iter().all(|(_, _, t)| t == "trusted" || t == "managed") {
        Ok("on".into())
    } else {
        Ok("untrusted".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_keeps_user_hooks() {
        let existing = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}}"#;
        let v = rewrite_hooks_json(Some(existing), Some("'/x/loopme-daemon' hook --loopme-data-dir '/d'")).unwrap();
        let stop = v["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["hooks"][0]["command"], "say done");
        assert_eq!(v["hooks"]["PermissionRequest"][0]["hooks"][0]["timeout"], 900);
        // Installing twice does not duplicate.
        let again = rewrite_hooks_json(Some(&v.to_string()), Some("'/y/loopme-daemon' hook --loopme-data-dir '/d'")).unwrap();
        assert_eq!(again["hooks"]["Stop"].as_array().unwrap().len(), 2);
        assert_eq!(again["hooks"]["Stop"][1]["hooks"][0]["command"], "'/y/loopme-daemon' hook --loopme-data-dir '/d'");
        // Uninstall leaves only the user's hook.
        let off = rewrite_hooks_json(Some(&again.to_string()), None).unwrap();
        assert_eq!(off, serde_json::json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"say done"}]}]}}));
    }

    #[test]
    fn rewrite_refuses_bad_json() {
        assert!(rewrite_hooks_json(Some("{oops"), None).is_err());
        assert!(rewrite_hooks_json(None, None).is_ok());
    }

    #[test]
    fn compact_event_extracts_command() {
        let raw = serde_json::json!({"hook_event_name":"PermissionRequest","session_id":"s","cwd":"/r","tool_name":"Bash",
            "tool_input":{"command":["npm","install","vitest"]},"transcript_path":null,"turn_id":"t","model":"m","permission_mode":"default"});
        let e = compact_event(&raw, "hk1", 5);
        assert_eq!(e["toolInput"], "npm install vitest");
        assert_eq!(e["event"], "PermissionRequest");
        assert_eq!(e["sessionId"], "s");
    }

    #[test]
    fn quoting() {
        assert_eq!(hook_command(Path::new("/A B/x"), Path::new("/d'q")), "'/A B/x' hook --loopme-data-dir '/d'\\''q'");
    }
}
