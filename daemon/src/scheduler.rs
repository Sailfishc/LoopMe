//! Periodic work: morning brief, due item checks, pause and decision expiry,
//! and delivering notifications deferred by quiet hours.

use crate::app::{App, RunKind};
use crate::model::*;
use chrono::{Datelike, Local, NaiveTime, TimeZone, Weekday};
use std::sync::Arc;
use std::time::Duration;

pub async fn run(app: Arc<App>) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    let mut probe_every = 0u32;
    loop {
        tick.tick().await;
        if probe_every % 20 == 0 {
            app.refresh_codex_status().await;
        }
        probe_every = probe_every.wrapping_add(1);
        if let Err(e) = tick_once(&app).await {
            tracing::warn!("scheduler: {e:#}");
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

    if brief_due(app, &settings, Local::now()) {
        app.store.kv_set("last_brief_date", &Local::now().format("%Y-%m-%d").to_string())?;
        app.queue(RunKind::Brief { manual: false });
    }

    for item in app.store.due_items(now)? {
        app.queue(RunKind::Item { item_id: item.id });
    }
    Ok(())
}

fn brief_time(settings: &Settings) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(&settings.brief_time, "%H:%M").ok()
}

fn is_weekday(d: Weekday) -> bool {
    !matches!(d, Weekday::Sat | Weekday::Sun)
}

fn brief_due(app: &App, settings: &Settings, now: chrono::DateTime<Local>) -> bool {
    if !settings.brief_enabled {
        return false;
    }
    let Some(t) = brief_time(settings) else { return false };
    if settings.brief_weekdays_only && !is_weekday(now.weekday()) {
        return false;
    }
    if now.time() < t {
        return false;
    }
    // Do not fire a stale brief late in the day (e.g. the Mac woke at 18:00).
    if now.time() > t + chrono::Duration::hours(3) {
        return false;
    }
    let today = now.format("%Y-%m-%d").to_string();
    app.store.kv_get("last_brief_date").ok().flatten().as_deref() != Some(today.as_str())
}

/// The next time something will run on its own (shown in the Mac panel).
pub fn next_wake(app: &App, settings: &Settings) -> Option<i64> {
    if settings.paused {
        return settings.paused_until;
    }
    let item = app.store.next_check_time().ok().flatten();
    let brief = next_brief(settings, Local::now());
    match (item, brief) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

fn next_brief(settings: &Settings, now: chrono::DateTime<Local>) -> Option<i64> {
    if !settings.brief_enabled {
        return None;
    }
    let t = brief_time(settings)?;
    for add in 0..8 {
        let day = now.date_naive() + chrono::Duration::days(add);
        if settings.brief_weekdays_only && !is_weekday(day.weekday()) {
            continue;
        }
        let at = Local.from_local_datetime(&day.and_time(t)).earliest()?;
        if at > now {
            return Some(at.timestamp_millis());
        }
    }
    None
}
