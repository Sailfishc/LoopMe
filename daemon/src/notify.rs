//! Notifications: rules (kinds, quiet hours, rate limits) and direct APNs delivery.
//!
//! APNs is configured with `<data-dir>/apns.json`:
//! `{"teamId":"…","keyId":"…","keyPath":"/path/AuthKey_XXXX.p8","topic":"com.example.loopme"}`.

use crate::app::App;
use crate::model::*;
use crate::store::Notification;
use anyhow::{bail, Context, Result};
use chrono::Timelike;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Mutex;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApnsConfig {
    pub team_id: String,
    pub key_id: String,
    pub key_path: String,
    /// The iOS app's bundle id.
    pub topic: String,
}

pub struct Notifier {
    cfg: Option<ApnsConfig>,
    key: Option<jsonwebtoken::EncodingKey>,
    http: reqwest::Client,
    jwt: Mutex<Option<(String, i64)>>,
    pub load_error: Option<String>,
}

impl Notifier {
    pub fn load(data_dir: &Path) -> Self {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
            .unwrap_or_default();
        let mut n = Self { cfg: None, key: None, http, jwt: Mutex::new(None), load_error: None };
        let path = data_dir.join("apns.json");
        if !path.exists() {
            return n;
        }
        let res: Result<(ApnsConfig, jsonwebtoken::EncodingKey)> = (|| {
            let cfg: ApnsConfig = serde_json::from_slice(&std::fs::read(&path)?).context("apns.json 格式错误")?;
            let key_path = crate::codex::expand_home(&cfg.key_path);
            let pem = std::fs::read(&key_path).with_context(|| format!("读不到 {}", key_path.display()))?;
            let key = jsonwebtoken::EncodingKey::from_ec_pem(&pem).context(".p8 密钥无效")?;
            Ok((cfg, key))
        })();
        match res {
            Ok((cfg, key)) => {
                n.cfg = Some(cfg);
                n.key = Some(key);
            }
            Err(e) => {
                tracing::warn!("APNs disabled: {e:#}");
                n.load_error = Some(format!("{e:#}"));
            }
        }
        n
    }

    pub fn configured(&self) -> bool {
        self.cfg.is_some()
    }

    fn token(&self) -> Result<String> {
        let (Some(cfg), Some(key)) = (&self.cfg, &self.key) else { bail!("APNs 未配置") };
        let now = chrono::Utc::now().timestamp();
        let mut cache = self.jwt.lock().unwrap();
        if let Some((t, iat)) = cache.as_ref() {
            if now - iat < 50 * 60 {
                return Ok(t.clone());
            }
        }
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::ES256);
        header.kid = Some(cfg.key_id.clone());
        let t = jsonwebtoken::encode(&header, &json!({"iss": cfg.team_id, "iat": now}), key)?;
        *cache = Some((t.clone(), now));
        Ok(t)
    }

    /// Sends one push. Returns `Ok(false)` when the token is no longer valid.
    pub async fn send(&self, device_token: &str, env: &str, payload: &Value, collapse_id: Option<&str>) -> Result<bool> {
        let Some(cfg) = &self.cfg else { bail!("APNs 未配置") };
        let host = if env == "production" { "api.push.apple.com" } else { "api.sandbox.push.apple.com" };
        let mut req = self
            .http
            .post(format!("https://{host}/3/device/{device_token}"))
            .bearer_auth(self.token()?)
            .header("apns-topic", &cfg.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .json(payload);
        if let Some(c) = collapse_id {
            req = req.header("apns-collapse-id", c);
        }
        let resp = req.send().await?;
        let status = resp.status();
        if status.is_success() {
            return Ok(true);
        }
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 410 || body.contains("BadDeviceToken") || body.contains("Unregistered") {
            return Ok(false);
        }
        bail!("APNs {status}: {body}")
    }
}

fn kind_enabled(s: &NotifySettings, kind: &str) -> bool {
    match kind {
        "decision" => s.decision,
        "result" => s.result,
        "brief" => s.brief,
        "failure" => s.failure,
        _ => true,
    }
}

fn parse_hm(s: &str) -> Option<u32> {
    let (h, m) = s.split_once(':')?;
    Some(h.trim().parse::<u32>().ok()? * 60 + m.trim().parse::<u32>().ok()?)
}

pub fn in_quiet_hours(settings: &Settings, now: chrono::DateTime<chrono::Local>) -> bool {
    let (Some(start), Some(end)) = (parse_hm(&settings.quiet_start), parse_hm(&settings.quiet_end)) else {
        return false;
    };
    if start == end {
        return false;
    }
    let t = now.hour() * 60 + now.minute();
    if start < end {
        t >= start && t < end
    } else {
        t >= start || t < end
    }
}

/// Queues a notification and delivers it now if the rules allow.
pub async fn notify(app: &App, kind: &str, item_id: Option<&str>, title: &str, body: &str, extra: Value) {
    let settings = app.store.settings().unwrap_or_default();
    if !kind_enabled(&settings.notify, kind) {
        return;
    }
    let (shown_title, shown_body) = if settings.lock_screen_summary {
        (title.to_string(), truncate(body, 120))
    } else if kind == "decision" {
        ("LoopMe".to_string(), "有一件事需要你决定".to_string())
    } else {
        ("LoopMe".to_string(), "你的 Agent 有新消息".to_string())
    };
    let mut payload = json!({
        "aps": {"alert": {"title": shown_title, "body": shown_body}, "sound": "default", "thread-id": "loopme"},
        "kind": kind,
    });
    if kind == "decision" {
        payload["aps"]["category"] = json!("LOOPME_DECISION");
    }
    if let Value::Object(m) = extra {
        for (k, v) in m {
            payload[k] = v;
        }
    }
    let n = Notification {
        id: new_id("ntf"),
        kind: kind.into(),
        item_id: item_id.map(Into::into),
        title: title.into(),
        body: body.into(),
        payload: payload.to_string(),
        status: "pending".into(),
        created_at: now_ms(),
    };
    if app.store.insert_notification(&n).is_err() {
        return;
    }
    // At the Mac the menu bar and the "现在" bar already show it; the phone stays quiet.
    if app.at_desk() {
        let _ = app.store.set_notification_status(&n.id, "local");
        return;
    }
    // Quiet hours hold everything except approvals, which block work.
    if kind != "decision" && in_quiet_hours(&settings, chrono::Local::now()) {
        return; // delivered by `flush` after quiet hours
    }
    // Per-item daily limit (decisions are exempt: they block work).
    if let (Some(i), true) = (item_id, kind != "decision") {
        let day = app.store.sent_since(now_ms() - 24 * 3600 * 1000, Some(i)).unwrap_or(0);
        if day >= settings.limits.per_item_per_day {
            let _ = app.store.set_notification_status(&n.id, "dropped");
            return;
        }
    }
    let hour = app.store.sent_since(now_ms() - 3600 * 1000, None).unwrap_or(0);
    if hour >= settings.limits.per_hour {
        return; // stays pending; merged by `flush`
    }
    deliver(app, &n.id, &payload).await;
}

/// Delivers deferred notifications once quiet hours are over, merging several into one.
pub async fn flush(app: &App) {
    let settings = app.store.settings().unwrap_or_default();
    if in_quiet_hours(&settings, chrono::Local::now()) {
        return;
    }
    let pending = app.store.pending_notifications().unwrap_or_default();
    if pending.is_empty() {
        return;
    }
    let hour = app.store.sent_since(now_ms() - 3600 * 1000, None).unwrap_or(0);
    if hour >= settings.limits.per_hour {
        return;
    }
    if pending.len() == 1 {
        let n = &pending[0];
        let payload: Value = serde_json::from_str(&n.payload).unwrap_or(json!({}));
        deliver(app, &n.id, &payload).await;
        return;
    }
    let decisions = pending.iter().filter(|n| n.kind == "decision").count();
    let body = if decisions > 0 {
        format!("你有 {} 条新消息，其中 {} 件需要你决定", pending.len(), decisions)
    } else {
        format!("你有 {} 条新消息", pending.len())
    };
    let summary = Notification {
        id: new_id("ntf"),
        kind: "summary".into(),
        item_id: None,
        title: "LoopMe".into(),
        body: body.clone(),
        payload: String::new(),
        status: "pending".into(),
        created_at: now_ms(),
    };
    let payload = json!({"aps": {"alert": {"title": "LoopMe", "body": body}, "sound": "default", "thread-id": "loopme"}, "kind": "summary"});
    let _ = app.store.insert_notification(&Notification { payload: payload.to_string(), ..summary.clone() });
    for n in &pending {
        let _ = app.store.set_notification_status(&n.id, "merged");
    }
    deliver(app, &summary.id, &payload).await;
}

async fn deliver(app: &App, notification_id: &str, payload: &Value) {
    let devices = app.store.list_devices().unwrap_or_default();
    for d in devices.iter().filter(|d| !d.is_local) {
        let Some(token) = &d.push_token else { continue };
        let env = d.push_env.as_deref().unwrap_or("sandbox");
        match app.notifier.send(token, env, payload, None).await {
            Ok(true) => {}
            Ok(false) => {
                let mut d = d.clone();
                d.push_token = None;
                let _ = app.store.save_device(&d);
                tracing::info!("push token for {} is no longer valid", d.name);
            }
            Err(e) => tracing::warn!("push to {} failed: {e:#}", d.name),
        }
    }
    // Marked sent even when no device was reachable, so rate limits stay predictable.
    let _ = app.store.set_notification_status(notification_id, "sent");
}

/// Sends a test push to every device; returns a short report.
pub async fn test_push(app: &App) -> Result<String> {
    if !app.notifier.configured() {
        bail!(
            "APNs 未配置：{}",
            app.notifier.load_error.clone().unwrap_or_else(|| "请在数据目录放置 apns.json".into())
        );
    }
    let payload = json!({"aps": {"alert": {"title": "LoopMe", "body": "测试推送：连接正常"}, "sound": "default"}, "kind": "test"});
    let mut report = vec![];
    for d in app.store.list_devices()?.iter().filter(|d| !d.is_local) {
        let Some(token) = &d.push_token else {
            report.push(format!("{}：未注册推送", d.name));
            continue;
        };
        match app.notifier.send(token, d.push_env.as_deref().unwrap_or("sandbox"), &payload, None).await {
            Ok(true) => {
                report.push(format!("{}：已发送", d.name));
            }
            Ok(false) => report.push(format!("{}：推送令牌失效", d.name)),
            Err(e) => report.push(format!("{}：{e:#}", d.name)),
        }
    }
    if report.is_empty() {
        bail!("还没有配对的手机");
    }
    Ok(report.join("\n"))
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn quiet_hours_wraps_midnight() {
        let s = Settings::default(); // 22:00–08:00
        let at = |h, m| chrono::Local.with_ymd_and_hms(2026, 11, 5, h, m, 0).unwrap();
        assert!(in_quiet_hours(&s, at(23, 0)));
        assert!(in_quiet_hours(&s, at(7, 59)));
        assert!(!in_quiet_hours(&s, at(8, 0)));
        assert!(!in_quiet_hours(&s, at(12, 0)));
    }
}
