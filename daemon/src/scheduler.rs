//! Background work: the day's brief, due item checks, pause and decision
//! expiry, deferred notifications, presence, Codex hook events and the
//! signals derived from them.

use crate::app::{App, RunKind};
use crate::model::*;
use chrono::{Datelike, Local, Timelike, Weekday};
use std::sync::Arc;
use std::time::Duration;

/// Earliest local hour at which arriving at the Mac starts the day's brief.
const BRIEF_FROM_HOUR: u32 = 5;
/// How often LoopMe re-checks that its Codex hooks are still installed and trusted.
const HOOKS_CHECK_EVERY: u32 = 20; // ticks of 30 s

pub async fn run(app: Arc<App>) {
    tokio::spawn(presence_loop(app.clone()));
    tokio::spawn(inbox_loop(app.clone()));
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    let mut n = 0u32;
    loop {
        tick.tick().await;
        if n % 20 == 0 {
            app.refresh_codex_status().await;
        }
        if n % HOOKS_CHECK_EVERY == 1 {
            maintain_hooks(&app).await;
        }
        n = n.wrapping_add(1);
        if let Err(e) = tick_once(&app).await {
            tracing::warn!("scheduler: {e:#}");
        }
    }
}

/// Picks up Codex hook events as they arrive.
async fn inbox_loop(app: Arc<App>) {
    let _ = std::fs::create_dir_all(crate::hooks::inbox_dir(&app.data_dir).join("resp"));
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    loop {
        tick.tick().await;
        crate::sessions::ingest(&app).await;
    }
}

async fn presence_loop(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        let desk = {
            let dir = app.data_dir.clone();
            tokio::task::spawn_blocking(move || crate::presence::at_desk(&dir)).await.unwrap_or(false)
        };
        if app.set_at_desk(desk) {
            // Back at the Mac: Codex asks there again, and the day starts.
            crate::sessions::release_session_approvals(&app);
            maybe_start_brief(&app);
        }
        // Signals are checked often so a merged "stopped" signal is not late.
        let settings = app.store.settings().unwrap_or_default();
        if let Err(e) = crate::sessions::tick(&app, &settings) {
            tracing::warn!("signals: {e:#}");
        }
    }
}

pub async fn tick_once(app: &Arc<App>) -> anyhow::Result<()> {
    let now = now_ms();
    let mut settings = app.store.settings()?;

    if settings.paused {
        if let Some(until) = settings.paused_until {
            if until <= now {
                settings.paused = false;
                settings.paused_until = None;
                app.store.set_json("settings", &settings)?;
                app.emit("settings", serde_json::json!(settings));
                app.emit_status();
            }
        }
    }

    for d in app.store.pending_decisions()? {
        if d.expires_at <= now {
            let _ = crate::engine::answer_decision(app, &d.id, None).await;
        }
    }

    crate::notify::flush(app).await;

    if settings.paused {
        return Ok(());
    }

    if app.at_desk() {
        maybe_start_brief(app);
    }

    for item in app.store.due_items(now)? {
        app.queue(RunKind::Item { item_id: item.id });
    }
    Ok(())
}

fn is_weekday(d: Weekday) -> bool {
    !matches!(d, Weekday::Sat | Weekday::Sun)
}

pub fn brief_due(settings: &Settings, last_brief_date: Option<&str>, now: chrono::DateTime<Local>) -> bool {
    if !settings.brief_enabled || settings.paused {
        return false;
    }
    if settings.brief_weekdays_only && !is_weekday(now.weekday()) {
        return false;
    }
    if now.hour() < BRIEF_FROM_HOUR {
        return false;
    }
    last_brief_date != Some(now.format("%Y-%m-%d").to_string().as_str())
}

/// Starts today's brief the first time the user shows up (at the Mac or in the app).
pub fn maybe_start_brief(app: &App) {
    let settings = app.store.settings().unwrap_or_default();
    let now = Local::now();
    let last = app.store.kv_get("last_brief_date").ok().flatten();
    if brief_due(&settings, last.as_deref(), now) {
        let _ = app.store.kv_set("last_brief_date", &now.format("%Y-%m-%d").to_string());
        app.queue(RunKind::Brief { manual: false });
    }
}

/// The user's one-time confirm: install LoopMe's hooks and trust them in Codex.
pub async fn enable_hooks(app: &Arc<App>) -> anyhow::Result<String> {
    let exe = std::env::current_exe()?;
    crate::hooks::install(&exe, &app.data_dir)?;
    let client = crate::engine::ensure_codex(app).await?;
    crate::hooks::trust(&client).await?;
    let state = crate::hooks::status(&client).await?;
    let mut settings = app.store.settings()?;
    settings.codex_hooks = true;
    app.store.set_json("settings", &settings)?;
    app.emit("settings", serde_json::json!(settings));
    app.set_hooks_status(&state);
    Ok(state)
}

/// Keeps the hooks working after the app moves or updates (the user already agreed once).
async fn maintain_hooks(app: &Arc<App>) {
    let settings = app.store.settings().unwrap_or_default();
    if !settings.codex_hooks {
        app.set_hooks_status("off");
        return;
    }
    let codex = app.status.lock().unwrap().codex.clone();
    if !codex.found {
        return;
    }
    let res: anyhow::Result<String> = async {
        let client = crate::engine::ensure_codex(app).await?;
        let mut state = crate::hooks::status(&client).await?;
        if state != "on" {
            crate::hooks::install(&std::env::current_exe()?, &app.data_dir)?;
            crate::hooks::trust(&client).await?;
            state = crate::hooks::status(&client).await?;
        }
        Ok(state)
    }
    .await;
    match res {
        Ok(s) => app.set_hooks_status(&s),
        Err(e) => {
            tracing::warn!("codex hooks: {e:#}");
            app.set_hooks_status("error");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn brief_once_a_day_after_five() {
        let s = Settings::default();
        let thu = |h| Local.with_ymd_and_hms(2026, 11, 5, h, 0, 0).unwrap();
        assert!(!brief_due(&s, None, thu(4)));
        assert!(brief_due(&s, None, thu(7)));
        assert!(brief_due(&s, Some("2026-11-04"), thu(19)));
        assert!(!brief_due(&s, Some("2026-11-05"), thu(9)));
        let sat = Local.with_ymd_and_hms(2026, 11, 7, 9, 0, 0).unwrap();
        assert!(!brief_due(&s, None, sat));
        let off = Settings { brief_enabled: false, ..Default::default() };
        assert!(!brief_due(&off, None, thu(9)));
    }
}
