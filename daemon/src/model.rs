//! Domain model shared with the Swift clients (see `apple/LoopMeKit/Sources/LoopMeKit/Models.swift`).
//! JSON uses camelCase keys; timestamps are Unix milliseconds.

use serde::{Deserialize, Serialize};

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub seq: i64,
    /// `user`, `agent` or `system`.
    pub role: String,
    pub text: String,
    pub item_id: Option<String>,
    pub decision_id: Option<String>,
    pub created_at: i64,
    pub streaming: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: String,
    pub title: String,
    pub detail: String,
    /// `scheduled`, `active`, `needsYou`, `snoozed`, `done`, `dropped`, `suggested`.
    pub status: String,
    pub next_check_at: Option<i64>,
    pub done_when: Option<String>,
    pub source: Option<String>,
    pub checks: i64,
    pub unchanged_checks: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub id: String,
    pub item_id: Option<String>,
    pub title: String,
    pub detail: String,
    /// `approval` (a Codex approval request) or `question` (asked by the agent).
    pub kind: String,
    /// `pending`, `approved`, `rejected`, `expired`.
    pub status: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub answered_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub id: String,
    pub text: String,
    /// `agent` or `user`.
    pub author: String,
    /// `active`, `locked`, `deleted`.
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Profile {
    pub name: String,
    pub timezone: String,
    pub work_hours: String,
    pub language: String,
    pub about: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct NotifySettings {
    pub decision: bool,
    pub result: bool,
    pub brief: bool,
    pub failure: bool,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self { decision: true, result: true, brief: true, failure: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Limits {
    pub per_item_per_day: i64,
    pub per_hour: i64,
    pub decision_timeout_hours: i64,
    pub max_unchanged_checks: i64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { per_item_per_day: 2, per_hour: 5, decision_timeout_hours: 48, max_unchanged_checks: 10 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    /// `readOnly`, `workspace` (default) or `full`.
    pub permission_level: String,
    pub workspace_roots: Vec<String>,
    pub brief_enabled: bool,
    /// Local time, `HH:MM`.
    pub brief_time: String,
    pub brief_weekdays_only: bool,
    pub quiet_start: String,
    pub quiet_end: String,
    pub paused: bool,
    pub paused_until: Option<i64>,
    pub notify: NotifySettings,
    pub show_steps: bool,
    pub lock_screen_summary: bool,
    pub session_analysis: bool,
    pub codex_path: Option<String>,
    pub model: Option<String>,
    pub limits: Limits,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            permission_level: "workspace".into(),
            workspace_roots: vec![],
            brief_enabled: true,
            brief_time: "09:00".into(),
            brief_weekdays_only: true,
            quiet_start: "22:00".into(),
            quiet_end: "08:00".into(),
            paused: false,
            paused_until: None,
            notify: NotifySettings::default(),
            show_steps: false,
            lock_screen_summary: false,
            session_analysis: true,
            codex_path: None,
            model: None,
            limits: Limits::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexStatus {
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub logged_in: Option<bool>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunInfo {
    pub id: String,
    pub title: String,
    pub started_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub mac_name: String,
    pub daemon_version: String,
    pub codex: CodexStatus,
    /// `idle`, `working`, `paused`, `error`.
    pub agent: String,
    pub current_run: Option<RunInfo>,
    pub next_wake_at: Option<i64>,
    pub push_configured: bool,
    pub listen_port: u16,
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub is_local: bool,
    pub has_push: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub seq: i64,
    pub messages: Vec<Message>,
    pub items: Vec<Item>,
    pub decisions: Vec<Decision>,
    pub memory: Vec<MemoryEntry>,
    pub profile: Profile,
    pub settings: Settings,
    pub status: Status,
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PairingState {
    /// `idle`, `waiting` (QR shown), `confirm` (phone connected, SAS shown), `paired`, `failed`.
    pub state: String,
    pub url: Option<String>,
    pub sas: Option<String>,
    pub device_name: Option<String>,
    pub expires_at: Option<i64>,
    pub error: Option<String>,
}

impl Default for PairingState {
    fn default() -> Self {
        Self { state: "idle".into(), url: None, sas: None, device_name: None, expires_at: None, error: None }
    }
}

/// A server → client event. `kind` names the payload in `data`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub event: String,
    pub seq: i64,
    pub data: serde_json::Value,
}
