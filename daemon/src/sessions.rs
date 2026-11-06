//! The user's own Codex sessions, fed by Codex hooks (see `hooks.rs`), and the
//! signals LoopMe derives from them.
//!
//! Signals are cheap rules first and a read-only look by Codex only when one passes
//! (see `attention.rs` for what is shown and how loudly):
//! * a session stops -> looked at after a 2-minute merge window (`MERGE_MS`), so
//!   a burst of events becomes one look; at the Mac only after a long task;
//! * a session that has sat still for 4 hours -> "stalled", at most once a day;
//! * a permission request -> an `approval` card at once (and a decision on the
//!   phone when the user is away).

use crate::app::{App, RunKind};
use crate::hooks;
use crate::model::*;
use anyhow::Result;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;

pub const MERGE_MS: i64 = 2 * 60 * 1000;

/// The merge window; `LOOPME_MERGE_MS` shortens it in tests.
fn merge_ms() -> i64 {
    std::env::var("LOOPME_MERGE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(MERGE_MS)
}
pub const STALL_MS: i64 = 4 * 3600 * 1000;
/// Sessions shown in the "现在" bar and considered for signals.
pub const RECENT_MS: i64 = 24 * 3600 * 1000;
/// Looks by Codex per day, so a busy day doesn't eat the user's Codex quota.
const MAX_LOOKS_PER_DAY: i64 = 40;

fn under(dir: &str, path: &str) -> bool {
    let d = crate::codex::expand_home(dir);
    Path::new(path).starts_with(&d)
}

pub fn ignored(settings: &Settings, cwd: &str) -> bool {
    settings.ignored_dirs.iter().any(|d| !d.trim().is_empty() && under(d.trim(), cwd))
}

fn git_branch(cwd: &str) -> Option<String> {
    let out = std::process::Command::new("git").args(["-C", cwd, "rev-parse", "--abbrev-ref", "HEAD"]).output().ok()?;
    let b = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !b.is_empty() && b != "HEAD").then_some(b)
}

fn title_from(prompt: &str) -> String {
    let line = prompt.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    crate::notify::truncate(line, 40)
}

fn respond(app: &App, event_id: &str, body: Value) {
    let path = hooks::resp_path(&app.data_dir, event_id);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Reads new hook events from the inbox and applies them. Returns how many were read.
pub async fn ingest(app: &Arc<App>) -> usize {
    let dir = hooks::inbox_dir(&app.data_dir);
    let Ok(rd) = std::fs::read_dir(&dir) else { return 0 };
    let mut files: Vec<_> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let n = files.len();
    for f in files {
        let body = std::fs::read_to_string(&f).ok();
        let _ = std::fs::remove_file(&f);
        if let Some(ev) = body.and_then(|b| serde_json::from_str::<Value>(&b).ok()) {
            if let Err(e) = apply_event(app, &ev).await {
                tracing::warn!("hook event: {e:#}");
            }
        }
    }
    n
}

pub async fn apply_event(app: &Arc<App>, ev: &Value) -> Result<()> {
    let g = |k: &str| ev.get(k).and_then(Value::as_str).map(str::to_string);
    let (Some(event), Some(sid), Some(event_id)) = (g("event"), g("sessionId"), g("id")) else { return Ok(()) };
    let cwd = g("cwd").unwrap_or_default();
    let settings = app.store.settings()?;
    let own = app.store.own_codex_threads(500)?.contains(&sid);
    if own || !settings.codex_hooks || ignored(&settings, &cwd) {
        if event == "PermissionRequest" {
            respond(app, &event_id, json!({"wait": false}));
        }
        return Ok(());
    }
    let now = ev.get("at").and_then(Value::as_i64).unwrap_or_else(now_ms);
    let mut s = app.store.get_session(&sid)?.unwrap_or_else(|| CodexSession {
        id: sid.clone(),
        cwd: cwd.clone(),
        state: "idle".into(),
        started_at: now,
        ..Default::default()
    });
    if !cwd.is_empty() {
        s.cwd = cwd.clone();
    }
    if let Some(t) = g("transcriptPath") {
        s.transcript_path = Some(t);
    }
    s.updated_at = now;
    let away = !app.at_desk();
    match event.as_str() {
        "SessionStart" => {
            s.state = "idle".into();
            s.branch = git_branch(&s.cwd);
            crate::attention::clear(&mut s);
        }
        "UserPromptSubmit" => {
            s.state = "working".into();
            if let Some(p) = g("prompt") {
                // The first prompt names the session (until then it is named after its folder).
                if s.last_prompt.is_none() || s.title.is_empty() {
                    s.title = title_from(&p);
                }
                s.last_prompt = Some(p);
            }
            // The user is driving it: nothing to evaluate, and the old card is answered.
            s.eval_at = None;
            s.turn_started_at = Some(now);
            crate::attention::clear(&mut s);
        }
        "Stop" => {
            s.state = "idle".into();
            s.last_message = g("lastMessage");
            s.branch = git_branch(&s.cwd).or(s.branch.take());
            clear_approval(&mut s);
            if crate::attention::worth_judging(&s, now, !away) {
                s.eval_at = Some(now + merge_ms());
            }
        }
        "Interrupt" => {
            s.state = "idle".into();
            clear_approval(&mut s);
        }
        "SessionEnd" => {
            s.state = "ended".into();
        }
        "PermissionRequest" => {
            s.state = "waitingApproval".into();
            let what = g("toolInput").or(g("toolName")).unwrap_or_else(|| "一个操作".into());
            let att = Attention {
                kind: "approval".into(),
                level: "interrupt".into(),
                summary: format!("要执行：{}", crate::notify::truncate(&what, 80)),
                evidence: g("toolName").map(|t| format!("工具：{t}")),
                at: now,
                ..Default::default()
            };
            crate::attention::raise(app, &mut s, att).await;
            if away && settings.remote_approvals {
                session_approval(app, &s, &event_id, g("toolName"), g("toolInput")).await?;
                respond(app, &event_id, json!({"wait": true}));
            } else {
                respond(app, &event_id, json!({"wait": false}));
            }
        }
        _ => {}
    }
    if s.title.is_empty() {
        s.title = Path::new(&s.cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Codex 会话".into());
    }
    app.store.save_session(&s)?;
    app.emit("session", json!(s));
    Ok(())
}

/// The approval was answered (or Codex moved on): its card goes away.
fn clear_approval(s: &mut CodexSession) {
    if s.attention.as_ref().is_some_and(|a| a.kind == "approval") {
        s.attention = None;
    }
}

async fn session_approval(app: &Arc<App>, s: &CodexSession, event_id: &str, tool: Option<String>, input: Option<String>) -> Result<()> {
    let project = Path::new(&s.cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let what = input.clone().unwrap_or_else(|| tool.clone().unwrap_or_else(|| "一个操作".into()));
    let title = format!("你的 Codex 想在 {project} 里执行：{}", crate::notify::truncate(&what, 60));
    let mut detail = format!("会话：{}\n目录：{}", s.title, s.cwd);
    if let Some(t) = &tool {
        detail.push_str(&format!("\n工具：{t}"));
    }
    if let Some(i) = &input {
        detail.push_str(&format!("\n内容：{i}"));
    }
    detail.push_str("\n你不在 Mac 前，所以先问你。选「回 Mac 再说」会让 Codex 照常在 Mac 上问你。");
    let now = now_ms();
    let d = Decision {
        id: new_id("dec"),
        item_id: None,
        title: title.clone(),
        detail: detail.clone(),
        kind: "sessionApproval".into(),
        status: "pending".into(),
        created_at: now,
        expires_at: now + hooks::APPROVAL_WAIT_SECS as i64 * 1000 - 30_000,
        answered_at: None,
    };
    app.store.save_decision(&d)?;
    app.store.kv_set(&format!("hookreq:{}", d.id), event_id)?;
    app.emit_decision(&d);
    app.post("system", &title, None, Some(&d.id))?;
    crate::notify::notify(app, "decision", None, &title, &detail, json!({"decisionId": d.id})).await;
    Ok(())
}

/// Passes the phone's answer to the waiting hook. `None` = let Codex ask on the Mac.
pub fn answer_session_approval(app: &App, decision_id: &str, approved: Option<bool>) {
    let key = format!("hookreq:{decision_id}");
    if let Ok(Some(event_id)) = app.store.kv_get(&key) {
        let behavior = match approved {
            Some(true) => json!("allow"),
            Some(false) => json!("deny"),
            None => Value::Null,
        };
        respond(app, &event_id, json!({"behavior": behavior}));
        let _ = app.store.kv_delete(&key);
    }
}

/// The user came back to the Mac: hand pending session approvals back to Codex there.
pub fn release_session_approvals(app: &App) {
    for mut d in app.store.pending_decisions().unwrap_or_default() {
        if d.kind != "sessionApproval" {
            continue;
        }
        answer_session_approval(app, &d.id, None);
        d.status = "expired".into();
        d.answered_at = Some(now_ms());
        let _ = app.store.save_decision(&d);
        app.emit_decision(&d);
    }
}

/// Merged "stopped" signals and stalled sessions -> looks by Codex; stale approvals expire.
/// Called from the scheduler.
pub fn tick(app: &Arc<App>, settings: &Settings) -> Result<()> {
    if !settings.codex_hooks || settings.paused {
        return Ok(());
    }
    let now = now_ms();
    let away = !app.at_desk();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let offered_today = |s: &CodexSession| {
        s.offered_at.is_some_and(|t| {
            chrono::TimeZone::timestamp_millis_opt(&chrono::Local, t).single().map(|d| d.format("%Y-%m-%d").to_string()) == Some(today.clone())
        })
    };
    let looks_key = format!("session_looks:{today}");
    let mut looks: i64 = app.store.kv_get(&looks_key)?.and_then(|v| v.parse().ok()).unwrap_or(0);
    let sessions = app.store.sessions_since(now - 3 * 24 * 3600 * 1000)?;
    for mut s in sessions {
        // The hook gave up waiting (or the answer never came back): no longer 等授权.
        if s.state == "waitingApproval" && now - s.updated_at > crate::attention::STALE_APPROVAL_MS {
            s.state = "idle".into();
            clear_approval(&mut s);
            app.store.save_session(&s)?;
            app.emit("session", json!(s));
        }
        if s.muted || ignored(settings, &s.cwd) {
            continue;
        }
        let reason = if s.eval_at.is_some_and(|t| t <= now) {
            Some("stopped")
        } else if away
            && s.state == "idle"
            && s.attention.is_none()
            && !crate::attention::is_noise(&s)
            && now - s.updated_at >= STALL_MS
            && s.updated_at >= now - RECENT_MS * 2
            && !offered_today(&s)
        {
            Some("stalled")
        } else {
            None
        };
        let Some(reason) = reason else { continue };
        s.eval_at = None;
        if reason == "stalled" {
            s.offered_at = Some(now); // at most once a day
        }
        app.store.save_session(&s)?;
        if looks >= MAX_LOOKS_PER_DAY {
            continue;
        }
        looks += 1;
        app.queue(RunKind::Session { session_id: s.id.clone(), reason: reason.into() });
    }
    app.store.kv_set(&looks_key, &looks.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_dirs_match_prefix() {
        let s = Settings { ignored_dirs: vec!["/Users/me/private".into(), "".into()], ..Default::default() };
        assert!(ignored(&s, "/Users/me/private/notes"));
        assert!(!ignored(&s, "/Users/me/privateer"));
        assert!(!ignored(&s, "/Users/me/work"));
    }

    #[test]
    fn titles_are_short_first_lines() {
        assert_eq!(title_from("\n修一下登录测试\n细节"), "修一下登录测试");
    }
}
