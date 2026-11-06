//! What the user should know about their own Codex sessions, and how loudly.
//!
//! Every time a session stops, LoopMe decides (rules first, then a read-only
//! look by Codex) whether the user needs to act or has a result:
//!
//! * needs you (`approval`, `question`, `stuck`) -> **interrupt**:
//!   a phone push when away, a Mac banner when at the Mac, nothing while the
//!   user is watching a Codex window;
//! * result (`done`, `milestone`) -> **sync**: a card in the "现在" bar, no sound;
//! * anything else -> silent (only counted as running / idle).
//!
//! Rhythm: one session doesn't interrupt twice for the same thing within 30
//! minutes, at most 4 interrupts an hour and 20 a day (beyond that they become
//! cards), and quiet hours hold everything but approvals until morning
//! (`notify::flush`).

use crate::app::App;
use crate::model::*;
use anyhow::{anyhow, Result};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;

/// A turn that ran at least this long counts as a long task: its end is worth a look even at the Mac.
pub const LONG_TASK_MS: i64 = 10 * 60 * 1000;
/// The same session doesn't interrupt twice for the same kind within this window.
pub const REPEAT_MS: i64 = 30 * 60 * 1000;
pub const MAX_INTERRUPTS_PER_HOUR: usize = 4;
pub const MAX_INTERRUPTS_PER_DAY: usize = 20;
/// A permission request with no news for this long is no longer waiting (the hook has given up).
pub const STALE_APPROVAL_MS: i64 = 15 * 60 * 1000;
/// 稍后.
pub const SNOOZE_MINUTES: i64 = 30;

/// `interrupt` for things that need the user, `sync` for results, `None` for everything else.
pub fn level_for(kind: &str) -> Option<&'static str> {
    match kind {
        "approval" | "question" | "stuck" => Some("interrupt"),
        "done" | "milestone" => Some("sync"),
        _ => None,
    }
}

pub fn label(kind: &str) -> &'static str {
    match kind {
        "approval" => "等授权",
        "question" => "在问你",
        "stuck" => "卡住",
        "done" => "完成",
        "milestone" => "阶段成果",
        _ => "会话",
    }
}

/// Automation and wrapper prompts (`<heartbeat>`, `<environment_context>` …) are never worth a look.
pub fn is_noise(s: &CodexSession) -> bool {
    let tagged = |t: &str| t.trim_start().starts_with('<');
    tagged(&s.title) || s.last_prompt.as_deref().is_some_and(tagged)
}

pub fn long_turn(s: &CodexSession, now: i64) -> bool {
    s.turn_started_at.is_some_and(|t| now - t >= LONG_TASK_MS)
}

/// Whether a session that just stopped deserves Codex's look. Away: always (it may be stuck or asking).
/// At the Mac: only after a long task; for a short one the user is right there.
pub fn worth_judging(s: &CodexSession, now: i64, at_desk: bool) -> bool {
    if s.muted || is_noise(s) {
        return false;
    }
    !at_desk || long_turn(s, now)
}

/// `项目 · 分支`, the name the user knows the session by.
pub fn project(s: &CodexSession) -> String {
    let name = Path::new(&s.cwd).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| s.title.clone());
    match &s.branch {
        Some(b) if !b.is_empty() => format!("{name} · {b}"),
        _ => name,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Only the card in the "现在" bar changes.
    Card,
    /// A Mac notification (the user is at the Mac but not watching Codex).
    Banner,
    /// A phone push.
    Push,
}

pub fn route(level: &str, at_desk: bool, watching: bool) -> Route {
    if level != "interrupt" || watching {
        Route::Card
    } else if at_desk {
        Route::Banner
    } else {
        Route::Push
    }
}

/// The rhythm rules for one interrupt. `log` holds the times of recent interrupts and is updated.
pub fn allow_interrupt(s: &CodexSession, kind: &str, now: i64, log: &mut Vec<i64>) -> bool {
    // Every approval is a new request that blocks work, so it is never held back.
    if kind != "approval" {
        if s.alerted_kind.as_deref() == Some(kind) && s.alerted_at.is_some_and(|t| now - t < REPEAT_MS) {
            return false;
        }
        log.retain(|t| now - t < 24 * 3600 * 1000);
        let hour = log.iter().filter(|t| now - **t < 3600 * 1000).count();
        if hour >= MAX_INTERRUPTS_PER_HOUR || log.len() >= MAX_INTERRUPTS_PER_DAY {
            return false;
        }
    }
    log.push(now);
    true
}

/// Puts `att` on the session and tells the user as loudly as the rules allow. The caller saves the session.
/// Approval pushes are sent by the decision flow (`sessions::session_approval`), so here they only make a banner.
pub async fn raise(app: &Arc<App>, s: &mut CodexSession, att: Attention) {
    let now = att.at;
    let kind = att.kind.clone();
    let title = format!("{} · {}", label(&kind), project(s));
    let body = att.summary.clone();
    let level = att.level.clone();
    s.attention = Some(att);
    let at_desk = app.at_desk();
    let watching = at_desk && crate::presence::watching_codex(&app.data_dir);
    let r = route(&level, at_desk, watching);
    if r == Route::Card {
        return;
    }
    let allowed = {
        let mut log = app.interrupts.lock().unwrap();
        allow_interrupt(s, &kind, now, &mut log)
    };
    if !allowed {
        return;
    }
    s.alerted_kind = Some(kind.clone());
    s.alerted_at = Some(now);
    match r {
        Route::Banner => app.emit("alert", json!({"sessionId": s.id, "kind": kind, "title": title, "body": body})),
        Route::Push if kind != "approval" => {
            crate::notify::notify(app, "session", None, &title, &body, json!({"sessionId": s.id})).await;
        }
        _ => {}
    }
}

/// A stopped session whose look found nothing: the old card (if any) goes away.
pub fn clear(s: &mut CodexSession) {
    s.attention = None;
}

/// 好的 / 知道了: the card leaves the bar and stays in the chat as a one-line record.
pub fn ack(app: &App, session_id: &str) -> Result<CodexSession> {
    let mut s = app.store.get_session(session_id)?.ok_or_else(|| anyhow!("找不到这个会话"))?;
    if let Some(a) = s.attention.take() {
        app.post("system", &format!("{} · {}：{}", label(&a.kind), project(&s), a.summary), None, None)?;
    }
    app.store.save_session(&s)?;
    app.emit("session", json!(s));
    Ok(s)
}

/// 稍后: hide the card for a while; it comes back by itself.
pub fn snooze(app: &App, session_id: &str, minutes: Option<i64>) -> Result<CodexSession> {
    let mut s = app.store.get_session(session_id)?.ok_or_else(|| anyhow!("找不到这个会话"))?;
    let until = now_ms() + minutes.unwrap_or(SNOOZE_MINUTES).clamp(1, 24 * 60) * 60_000;
    if let Some(a) = s.attention.as_mut() {
        a.snoozed_until = Some(until);
    }
    app.store.save_session(&s)?;
    app.emit("session", json!(s));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sess() -> CodexSession {
        CodexSession { id: "s1".into(), cwd: "/Users/me/DSH".into(), title: "修登录".into(), branch: Some("fix-login".into()), ..Default::default() }
    }

    #[test]
    fn levels() {
        assert_eq!(level_for("stuck"), Some("interrupt"));
        assert_eq!(level_for("done"), Some("sync"));
        assert_eq!(level_for("none"), None);
    }

    #[test]
    fn noise_and_long_turns() {
        let mut s = sess();
        s.last_prompt = Some("<heartbeat>".into());
        assert!(is_noise(&s));
        assert!(!worth_judging(&s, 0, false));
        let mut s = sess();
        s.turn_started_at = Some(0);
        assert!(worth_judging(&s, 1000, false), "away: always look");
        assert!(!worth_judging(&s, 1000, true), "at the Mac, short task: no");
        assert!(worth_judging(&s, LONG_TASK_MS, true), "at the Mac, long task: yes");
    }

    #[test]
    fn routes_by_presence() {
        assert_eq!(route("sync", false, false), Route::Card);
        assert_eq!(route("interrupt", false, false), Route::Push);
        assert_eq!(route("interrupt", true, false), Route::Banner);
        assert_eq!(route("interrupt", true, true), Route::Card);
    }

    #[test]
    fn rhythm() {
        let mut s = sess();
        let mut log = vec![];
        assert!(allow_interrupt(&s, "stuck", 0, &mut log));
        s.alerted_kind = Some("stuck".into());
        s.alerted_at = Some(0);
        assert!(!allow_interrupt(&s, "stuck", REPEAT_MS - 1, &mut log), "same thing again too soon");
        assert!(allow_interrupt(&s, "question", 1, &mut log), "something new is fine");
        let mut other = sess();
        other.id = "s2".into();
        assert!(allow_interrupt(&other, "stuck", 2, &mut log));
        assert!(allow_interrupt(&other, "done", 3, &mut log));
        assert!(!allow_interrupt(&other, "milestone", 4, &mut log), "4 an hour at most");
        assert!(allow_interrupt(&other, "approval", 5, &mut log), "approvals always pass");
        assert!(allow_interrupt(&other, "stuck", 3600 * 1000 + 10, &mut log), "next hour");
    }

    #[test]
    fn project_names() {
        assert_eq!(project(&sess()), "DSH · fix-login");
    }
}
