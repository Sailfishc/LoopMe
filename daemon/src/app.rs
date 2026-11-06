//! Shared daemon state: storage, identity, event fan-out, status and pairing.

use crate::codex::CodexClient;
use crate::crypto::{self, KeyPair};
use crate::model::*;
use crate::notify::Notifier;
use crate::store::{DeviceRecord, Store};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc, oneshot};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
const PAIRING_TTL_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone)]
pub enum RunKind {
    /// A user chat message, optionally replying to an earlier message.
    Chat { text: String, reply_to: Option<String> },
    /// The user's answer to a question the agent asked.
    Answer { decision_id: String, approved: bool },
    /// A scheduled check of one item.
    Item { item_id: String },
    /// Morning brief; `manual` posts even when there is little to say.
    Brief { manual: bool },
    /// A signal from one of the user's own Codex sessions (`stopped` or `stalled`).
    Session { session_id: String, reason: String },
    /// The user tapped 让我来 on an offer card.
    Takeover { message_id: String },
}

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub kind: RunKind,
    pub generation: u64,
}

/// A Codex approval request waiting for the user's answer.
pub struct PendingApproval {
    pub client: Arc<CodexClient>,
    pub request_id: Value,
    pub turn_id: Option<String>,
}

#[derive(Default)]
pub struct EngineState {
    pub codex: tokio::sync::Mutex<Option<Arc<CodexClient>>>,
    /// (client, thread id, turn id) of the running turn.
    pub current: Mutex<Option<(Arc<CodexClient>, String, String)>>,
    pub approvals: Mutex<HashMap<String, PendingApproval>>,
    /// Bumped by `run.stop`; queued runs from older generations are skipped.
    pub generation: AtomicU64,
    /// Items that already have a queued or running check.
    pub queued_items: Mutex<std::collections::HashSet<String>>,
}

#[derive(Default)]
pub struct PairingSession {
    pub public: PairingState,
    pub code: Option<String>,
    pub confirm: Option<oneshot::Sender<bool>>,
}

pub struct App {
    pub store: Store,
    pub data_dir: PathBuf,
    pub identity: KeyPair,
    pub port: u16,
    pub events: broadcast::Sender<Event>,
    /// Device ids whose connections must close (revoked).
    pub kicks: broadcast::Sender<String>,
    pub status: Mutex<Status>,
    pub pairing: Mutex<PairingSession>,
    pub runs: mpsc::UnboundedSender<RunRequest>,
    pub engine: EngineState,
    pub notifier: Notifier,
    /// How phones reach this Mac when a relay is used (set once at startup).
    pub relay: Mutex<Option<crate::relay_link::LinkConfig>>,
}

impl App {
    pub fn new(data_dir: &Path, port: u16) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<RunRequest>)> {
        std::fs::create_dir_all(data_dir)?;
        let store = Store::open(&data_dir.join("loopme.db"))?;
        store.finish_streaming_messages()?;
        let identity = match store.kv_get("identity_secret")? {
            Some(s) => KeyPair::from_secret_bytes(crypto::key32(&s)?),
            None => {
                let k = KeyPair::generate();
                store.kv_set("identity_secret", &crypto::b64(&k.secret_bytes()))?;
                k
            }
        };
        let (events, _) = broadcast::channel(1024);
        let (kicks, _) = broadcast::channel(16);
        let (runs, rx) = mpsc::unbounded_channel();
        let notifier = Notifier::load(data_dir);
        let status = Status {
            mac_name: mac_name(),
            daemon_version: VERSION.into(),
            agent: "idle".into(),
            hooks: "off".into(),
            relay: "off".into(),
            relay_port: port,
            push_configured: notifier.configured(),
            listen_port: port,
            hosts: vec![],
            ..Default::default()
        };
        let app = Arc::new(Self {
            store,
            data_dir: data_dir.to_path_buf(),
            identity,
            port,
            events,
            kicks,
            status: Mutex::new(status),
            pairing: Mutex::new(PairingSession::default()),
            runs,
            engine: EngineState::default(),
            notifier,
            relay: Mutex::new(None),
        });
        Ok((app, rx))
    }

    pub fn emit(&self, event: &str, data: Value) {
        let seq = self.store.next_seq().unwrap_or(0);
        self.emit_seq(event, seq, data);
    }

    pub fn emit_seq(&self, event: &str, seq: i64, data: Value) {
        let _ = self.events.send(Event { event: event.into(), seq, data });
    }

    pub fn emit_message(&self, m: &Message) {
        self.emit_seq("message", m.seq, json!(m));
    }

    pub fn emit_item(&self, i: &Item) {
        self.emit("item", json!(i));
    }

    pub fn emit_decision(&self, d: &Decision) {
        self.emit("decision", json!(d));
    }

    pub fn emit_memory(&self) {
        if let Ok(m) = self.store.list_memory() {
            self.emit("memory", json!(m));
        }
    }

    pub fn emit_devices(&self) {
        self.emit("devices", json!(self.devices()));
    }

    pub fn emit_status(&self) {
        let s = self.status();
        self.emit("status", json!(s));
    }

    /// Posts a message to the single chat stream.
    pub fn post(&self, role: &str, text: &str, item_id: Option<&str>, decision_id: Option<&str>) -> Result<Message> {
        let m = self.store.insert_message(role, text, item_id, decision_id, false)?;
        self.emit_message(&m);
        Ok(m)
    }

    pub fn status(&self) -> Status {
        let mut s = self.status.lock().unwrap().clone();
        let settings = self.store.settings().unwrap_or_default();
        if settings.paused && s.agent == "idle" {
            s.agent = "paused".into();
        }
        s
    }

    pub fn at_desk(&self) -> bool {
        self.status.lock().unwrap().at_desk
    }

    /// Updates presence; returns true when the user just arrived at the Mac.
    pub fn set_at_desk(&self, at_desk: bool) -> bool {
        let arrived = {
            let mut s = self.status.lock().unwrap();
            let arrived = at_desk && !s.at_desk;
            let changed = s.at_desk != at_desk;
            s.at_desk = at_desk;
            if !changed {
                return false;
            }
            arrived
        };
        self.emit_status();
        arrived
    }

    /// Turns the relay on for phones (called once at startup).
    pub fn use_relay(&self, cfg: crate::relay_link::LinkConfig) {
        {
            let mut s = self.status.lock().unwrap();
            s.relay_port = cfg.public_port;
            s.relay_fingerprint = crate::relay::fingerprint_file(&self.data_dir);
        }
        *self.relay.lock().unwrap() = Some(cfg);
    }

    pub fn set_relay_status(&self, relay: &str) {
        {
            let mut s = self.status.lock().unwrap();
            if s.relay == relay {
                return;
            }
            s.relay = relay.into();
            if s.relay_fingerprint.is_none() {
                s.relay_fingerprint = crate::relay::fingerprint_file(&self.data_dir);
            }
        }
        self.emit_status();
    }

    /// Addresses for the QR code: manual ones from settings first, then this Mac's (VPN interfaces first).
    pub fn phone_hosts(&self) -> Vec<String> {
        let settings = self.store.settings().unwrap_or_default();
        let mut hosts: Vec<String> = settings.relay_hosts.iter().map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).collect();
        let remote = self.relay.lock().unwrap().as_ref().map(|c| c.public_hosts.clone()).unwrap_or_default();
        if remote.is_empty() {
            hosts.extend(advertised_hosts());
        } else {
            hosts.extend(remote);
        }
        let mut seen = std::collections::HashSet::new();
        hosts.retain(|h| seen.insert(h.clone()));
        hosts
    }

    pub fn set_hooks_status(&self, hooks: &str) {
        {
            let mut s = self.status.lock().unwrap();
            if s.hooks == hooks {
                return;
            }
            s.hooks = hooks.into();
        }
        self.emit_status();
    }

    /// Shows what the current run is doing (the "现在" bar's 正在做 line).
    pub fn set_run_step(&self, step: &str) {
        {
            let mut s = self.status.lock().unwrap();
            let Some(run) = s.current_run.as_mut() else { return };
            if run.step.as_deref() == Some(step) {
                return;
            }
            run.step = Some(step.into());
        }
        self.emit_status();
    }

    pub fn set_agent_state(&self, agent: &str, run: Option<RunInfo>) {
        {
            let mut s = self.status.lock().unwrap();
            s.agent = agent.into();
            s.current_run = run;
        }
        self.emit_status();
    }

    pub async fn refresh_codex_status(&self) {
        let settings = self.store.settings().unwrap_or_default();
        let st = crate::codex::probe(settings.codex_path.as_deref()).await;
        {
            let mut s = self.status.lock().unwrap();
            s.codex = st;
            s.hosts = self.phone_hosts();
            s.push_configured = self.notifier.configured();
        }
        self.emit_status();
    }

    pub fn devices(&self) -> Vec<Device> {
        self.store.list_devices().unwrap_or_default().iter().map(DeviceRecord::public).collect()
    }

    pub fn snapshot(&self) -> Result<Snapshot> {
        Ok(Snapshot {
            seq: self.store.current_seq()?,
            messages: self.store.recent_messages(300)?,
            items: self.store.list_items()?,
            decisions: self.store.list_decisions()?,
            memory: self.store.list_memory()?,
            profile: self.store.profile()?,
            settings: self.store.settings()?,
            status: self.status(),
            devices: self.devices(),
            sessions: self.store.sessions_since(now_ms() - crate::sessions::RECENT_MS)?,
        })
    }

    pub fn queue(&self, kind: RunKind) {
        if let RunKind::Item { item_id } = &kind {
            if !self.engine.queued_items.lock().unwrap().insert(item_id.clone()) {
                return;
            }
        }
        let generation = self.engine.generation.load(Ordering::SeqCst);
        let _ = self.runs.send(RunRequest { kind, generation });
    }

    // ---- local client ----

    /// Ensures the Mac app's own device exists and writes `local-client.json`.
    pub fn ensure_local_client(&self) -> Result<()> {
        let path = self.data_dir.join("local-client.json");
        let existing = self.store.list_devices()?.into_iter().find(|d| d.is_local);
        let secret_key = "local_client_secret";
        let (device, kp) = match (existing, self.store.kv_get(secret_key)?) {
            (Some(d), Some(s)) => (d, KeyPair::from_secret_bytes(crypto::key32(&s)?)),
            _ => {
                let kp = KeyPair::generate();
                let d = DeviceRecord {
                    id: new_id("dev"),
                    name: format!("{}（本机）", mac_name()),
                    public_key: kp.public_bytes(),
                    created_at: now_ms(),
                    last_seen_at: None,
                    is_local: true,
                    push_token: None,
                    push_env: None,
                };
                self.store.save_device(&d)?;
                self.store.kv_set(secret_key, &crypto::b64(&kp.secret_bytes()))?;
                (d, kp)
            }
        };
        let body = json!({
            "deviceId": device.id,
            "privateKey": crypto::b64(&kp.secret_bytes()),
            "macPublicKey": crypto::b64(&self.identity.public_bytes()),
            "port": self.port,
        });
        write_private(&path, serde_json::to_string_pretty(&body)?.as_bytes())?;
        Ok(())
    }

    // ---- pairing ----

    pub fn pairing_state(&self) -> PairingState {
        let mut p = self.pairing.lock().unwrap();
        if let Some(exp) = p.public.expires_at {
            if p.public.state == "waiting" && exp < now_ms() {
                p.public = PairingState { state: "idle".into(), ..Default::default() };
                p.code = None;
            }
        }
        p.public.clone()
    }

    pub fn start_pairing(&self) -> PairingState {
        let code = crypto::b64url(&crypto::random_bytes::<9>());
        let hosts = self.phone_hosts();
        let relay = self.relay.lock().unwrap().clone();
        let mut url = format!(
            "loopme://pair?v={}&k={}&c={}&h={}&p={}&n={}",
            if relay.is_some() { 2 } else { 1 },
            crypto::b64url(&self.identity.public_bytes()),
            code,
            hosts.join(","),
            relay.as_ref().map(|r| r.public_port).unwrap_or(self.port),
            crypto::b64url(mac_name().as_bytes()),
        );
        if relay.is_some() {
            // v2: phones go through the relay (`wss://host:p/v1/phone/<r>/…?t=<t>`, certificate pinned to `f`).
            url.push_str(&format!("&r={}&t={}", crate::relay_link::mac_id(self), crate::relay_link::phone_token(self)));
            if let Some(fp) = crate::relay::fingerprint_file(&self.data_dir) {
                url.push_str(&format!("&f={fp}"));
            }
        }
        let state = PairingState {
            state: "waiting".into(),
            url: Some(url),
            expires_at: Some(now_ms() + PAIRING_TTL_MS),
            ..Default::default()
        };
        {
            let mut p = self.pairing.lock().unwrap();
            p.public = state.clone();
            p.code = Some(code);
            p.confirm = None;
        }
        self.emit("pairing", json!(state));
        state
    }

    pub fn cancel_pairing(&self) {
        {
            let mut p = self.pairing.lock().unwrap();
            *p = PairingSession::default();
        }
        self.emit("pairing", json!(PairingState::default()));
    }

    /// Called when a phone presents a pairing code. Returns the SAS and a
    /// receiver that resolves once the user confirms or rejects on the Mac.
    pub fn pairing_request(&self, code: &str, device_pub: &[u8; 32], device_name: &str) -> Result<(String, oneshot::Receiver<bool>)> {
        let mut p = self.pairing.lock().unwrap();
        let valid = p.code.as_deref() == Some(code)
            && p.public.state == "waiting"
            && p.public.expires_at.map(|e| e > now_ms()).unwrap_or(false);
        if !valid {
            bail!("配对码无效或已过期");
        }
        let sas = crypto::pairing_sas(&self.identity.public_bytes(), device_pub);
        let (tx, rx) = oneshot::channel();
        p.confirm = Some(tx);
        p.code = None; // one phone per code
        p.public.state = "confirm".into();
        p.public.sas = Some(sas.clone());
        p.public.device_name = Some(device_name.to_string());
        p.public.expires_at = Some(now_ms() + 2 * 60 * 1000);
        let st = p.public.clone();
        drop(p);
        self.emit("pairing", json!(st));
        Ok((sas, rx))
    }

    pub fn confirm_pairing(&self, accept: bool) -> Result<()> {
        let tx = self.pairing.lock().unwrap().confirm.take().ok_or_else(|| anyhow!("没有等待确认的配对"))?;
        let _ = tx.send(accept);
        Ok(())
    }

    pub fn finish_pairing(&self, device: Option<&DeviceRecord>, error: Option<&str>) {
        let st = {
            let mut p = self.pairing.lock().unwrap();
            p.confirm = None;
            p.code = None;
            p.public = PairingState {
                state: if device.is_some() { "paired".into() } else { "failed".into() },
                device_name: device.map(|d| d.name.clone()).or(p.public.device_name.clone()),
                error: error.map(Into::into),
                ..Default::default()
            };
            p.public.clone()
        };
        self.emit("pairing", json!(st));
        if device.is_some() {
            self.emit_devices();
        }
    }

    pub fn revoke_device(&self, id: &str) -> Result<()> {
        if !self.store.delete_device(id)? {
            bail!("设备不存在或不能解除本机");
        }
        let _ = self.kicks.send(id.to_string());
        self.emit_devices();
        Ok(())
    }
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub fn mac_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        if let Ok(o) = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if o.status.success() && !s.is_empty() {
                return s;
            }
        }
        if let Ok(o) = std::process::Command::new("hostname").output() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
        "Mac".into()
    })
    .clone()
}

/// Addresses put in the pairing QR. `LOOPME_HOSTS` (comma separated) comes first.
pub fn advertised_hosts() -> Vec<String> {
    let mut hosts: Vec<String> = std::env::var("LOOPME_HOSTS")
        .ok()
        .map(|s| s.split(',').map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).collect())
        .unwrap_or_default();
    if let Ok(ifs) = if_addrs::get_if_addrs() {
        // Addresses on VPN interfaces first: a phone on the company VPN is most likely to reach those.
        let vpn = |name: &str| ["utun", "ppp", "ipsec", "tun", "tap", "wg"].iter().any(|p| name.starts_with(p));
        let mut v4: Vec<(bool, String)> = ifs
            .iter()
            .filter(|i| !i.is_loopback())
            .filter_map(|i| match i.ip() {
                std::net::IpAddr::V4(a) if !a.is_link_local() => Some((!vpn(&i.name), a.to_string())),
                _ => None,
            })
            .collect();
        v4.sort();
        v4.dedup();
        hosts.extend(v4.into_iter().map(|(_, a)| a));
    }
    if let Ok(o) = std::process::Command::new("scutil").args(["--get", "LocalHostName"]).output() {
        let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
        if o.status.success() && !s.is_empty() {
            hosts.push(format!("{s}.local"));
        }
    }
    let mut seen = std::collections::HashSet::new();
    hosts.retain(|h| seen.insert(h.clone()));
    hosts
}
