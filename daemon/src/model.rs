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
    /// The message this one replies to (quoted as context for the agent).
    #[serde(default)]
    pub reply_to: Option<String>,
    /// The user's reaction to an agent message: `up` or `down`.
    #[serde(default)]
    pub feedback: Option<String>,
    /// Makes the message an actionable card.
    #[serde(default)]
    pub offer: Option<Offer>,
}

/// An actionable card in the chat: every proactive message carries one.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct Offer {
    /// `takeover` (the agent can do it), `result` (a takeover finished), `info`.
    pub kind: String,
    /// Small label above the title, e.g. 可以接手 / 做完了.
    pub label: String,
    pub title: String,
    pub detail: String,
    /// Evidence shown behind 看依据.
    pub why: Option<String>,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    /// What the agent should do if the user taps 让我来.
    pub task: Option<String>,
    /// Set once a takeover started: the new branch, its base and the worktree.
    pub branch: Option<String>,
    pub base: Option<String>,
    pub worktree: Option<String>,
    pub actions: Vec<OfferAction>,
    /// `open`, `working`, `done` (result posted), `closed` (user chose).
    pub state: String,
    pub chosen: Option<String>,
    /// One line shown after a choice, e.g. 你选了：我自己来.
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct OfferAction {
    pub id: String,
    pub label: String,
    /// `primary`, `secondary` or `plain`.
    pub style: String,
}

impl OfferAction {
    pub fn new(id: &str, label: &str, style: &str) -> Self {
        Self { id: id.into(), label: label.into(), style: style.into() }
    }
}

/// One of the user's own Codex sessions, as seen through Codex hooks.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct CodexSession {
    pub id: String,
    pub cwd: String,
    pub title: String,
    pub branch: Option<String>,
    /// `working`, `waitingApproval`, `idle`, `ended`.
    pub state: String,
    pub last_prompt: Option<String>,
    pub last_message: Option<String>,
    pub transcript_path: Option<String>,
    pub started_at: i64,
    pub updated_at: i64,
    /// When a merged signal for this session is due for evaluation.
    pub eval_at: Option<i64>,
    pub offered_at: Option<i64>,
    /// The user said 我自己来 / 别管它: no more offers for this session.
    pub muted: bool,
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
    /// Current check interval in minutes; doubles while nothing changes (capped).
    #[serde(default)]
    pub interval_min: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub id: String,
    pub item_id: Option<String>,
    pub title: String,
    pub detail: String,
    /// `approval` (a Codex approval request in LoopMe's own run), `sessionApproval`
    /// (a permission request from the user's own Codex session while they are away),
    /// `question` (asked by the agent) or `continue`.
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
    /// Watch the user's own Codex sessions through Codex hooks (one-time confirm).
    pub codex_hooks: bool,
    /// Sessions under these directories are ignored.
    pub ignored_dirs: Vec<String>,
    /// While the user is away, their Codex permission requests can be answered from the phone.
    pub remote_approvals: bool,
    /// Extra addresses for the pairing QR code, tried first (e.g. a fixed intranet name for this Mac).
    pub relay_hosts: Vec<String>,
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
            codex_hooks: false,
            ignored_dirs: vec![],
            remote_approvals: true,
            relay_hosts: vec![],
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
    /// What the run is doing right now (e.g. the command being run).
    #[serde(default)]
    pub step: Option<String>,
    /// The chat message (offer card) this run belongs to.
    #[serde(default)]
    pub message_id: Option<String>,
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
    /// The user is at the Mac (recent keyboard or mouse input).
    pub at_desk: bool,
    /// Codex hooks: `off`, `on`, `untrusted` (needs the one-time confirm again) or `error`.
    pub hooks: String,
    pub push_configured: bool,
    pub listen_port: u16,
    pub hosts: Vec<String>,
    /// Link to the relay phones connect through: `off`, `connecting`, `on` or `error`.
    pub relay: String,
    /// The port phones use (the relay's public port, or `listen_port` without a relay).
    pub relay_port: u16,
    /// SHA-256 of the relay's certificate, pinned by phones.
    pub relay_fingerprint: Option<String>,
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
    /// The user's own Codex sessions active in the last day.
    pub sessions: Vec<CodexSession>,
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
