//! Client for `codex app-server` (newline-delimited JSON-RPC over stdio).
//!
//! The wire format omits the `"jsonrpc"` field. Requests carry `id` and
//! `method`; responses carry `id` and `result` or `error`; notifications carry
//! only `method`. The server also sends requests to us (approvals), which we
//! answer with `respond`.

use crate::model::CodexStatus;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, Clone)]
pub enum CodexEvent {
    Notification { method: String, params: Value },
    /// A request from the server that needs `respond(id, ...)`.
    Request { id: Value, method: String, params: Value },
    Exited,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>;

pub struct CodexClient {
    stdin: tokio::sync::Mutex<ChildStdin>,
    pending: Pending,
    next_id: AtomicI64,
    pub events: tokio::sync::Mutex<mpsc::UnboundedReceiver<CodexEvent>>,
    alive: Arc<std::sync::atomic::AtomicBool>,
    _child: tokio::sync::Mutex<Child>,
}

impl CodexClient {
    /// Starts `<codex> app-server` and performs the `initialize` handshake.
    pub async fn spawn(codex: &Path, version: &str) -> Result<Arc<Self>> {
        let mut cmd = Command::new(codex);
        cmd.arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            // Hooks fired by LoopMe's own Codex turns are ignored.
            .env(crate::hooks::INTERNAL_ENV, "1");
        // Make sure a login shell PATH is visible (node, git, etc. for the agent).
        if let Some(path) = login_path() {
            cmd.env("PATH", path);
        }
        let mut child = cmd.spawn().with_context(|| format!("failed to start {}", codex.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (tx, rx) = mpsc::unbounded_channel();
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

        {
            let pending = pending.clone();
            let alive = alive.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let line = line.trim();
                    if line.is_empty() {
                        continue;
                    }
                    let v: Value = match serde_json::from_str(line) {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!("codex: unparsable line ({e}): {line}");
                            continue;
                        }
                    };
                    route(&v, &pending, &tx);
                }
                alive.store(false, Ordering::SeqCst);
                for (_, s) in pending.lock().unwrap().drain() {
                    let _ = s.send(Err(anyhow!("codex app-server exited")));
                }
                let _ = tx.send(CodexEvent::Exited);
            });
        }
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("codex stderr: {line}");
            }
        });

        let client = Arc::new(Self {
            stdin: tokio::sync::Mutex::new(stdin),
            pending,
            next_id: AtomicI64::new(1),
            events: tokio::sync::Mutex::new(rx),
            alive,
            _child: tokio::sync::Mutex::new(child),
        });

        client
            .request(
                "initialize",
                json!({
                    "clientInfo": {"name": "loopme", "title": "LoopMe", "version": version},
                    "capabilities": null
                }),
            )
            .await
            .context("codex initialize failed")?;
        client.notify("initialized", None).await?;
        Ok(client)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    async fn write(&self, v: &Value) -> Result<()> {
        let mut s = serde_json::to_string(v)?;
        s.push('\n');
        let mut w = self.stdin.lock().await;
        w.write_all(s.as_bytes()).await?;
        w.flush().await?;
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_timeout(method, params, Duration::from_secs(60)).await
    }

    pub async fn request_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if !self.is_alive() {
            bail!("codex app-server is not running");
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.write(&json!({"id": id, "method": method, "params": params})).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => bail!("codex dropped request {method}"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                bail!("codex request {method} timed out")
            }
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let mut v = json!({"method": method});
        if let Some(p) = params {
            v["params"] = p;
        }
        self.write(&v).await
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<()> {
        self.write(&json!({"id": id, "result": result})).await
    }
}

fn route(v: &Value, pending: &Pending, tx: &mpsc::UnboundedSender<CodexEvent>) {
    let method = v.get("method").and_then(Value::as_str);
    let id = v.get("id");
    match (method, id) {
        (Some(m), Some(id)) => {
            let _ = tx.send(CodexEvent::Request {
                id: id.clone(),
                method: m.to_string(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        (Some(m), None) => {
            let _ = tx.send(CodexEvent::Notification {
                method: m.to_string(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
            });
        }
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else { return };
            let Some(sender) = pending.lock().unwrap().remove(&id) else { return };
            if let Some(err) = v.get("error") {
                let msg = err.get("message").and_then(Value::as_str).unwrap_or("error").to_string();
                let _ = sender.send(Err(anyhow!("codex error: {msg}")));
            } else {
                let _ = sender.send(Ok(v.get("result").cloned().unwrap_or(Value::Null)));
            }
        }
        (None, None) => {}
    }
}

/// PATH from the user's login shell; a LaunchAgent starts with a minimal PATH.
pub fn login_path() -> Option<String> {
    static CACHE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let out = std::process::Command::new(shell).args(["-lc", "printf %s \"$PATH\""]).output().ok()?;
            let p = String::from_utf8(out.stdout).ok()?;
            let p = p.trim().to_string();
            if p.is_empty() {
                None
            } else {
                Some(p)
            }
        })
        .clone()
}

/// Resolves the codex binary: setting, `LOOPME_CODEX`, login shell lookup, common locations.
pub fn resolve_codex(setting: Option<&str>) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![];
    if let Some(s) = setting.filter(|s| !s.trim().is_empty()) {
        candidates.push(expand_home(s));
    }
    if let Ok(s) = std::env::var("LOOPME_CODEX") {
        candidates.push(expand_home(&s));
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
    if let Ok(out) = std::process::Command::new(&shell).args(["-lc", "command -v codex"]).output() {
        let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !p.is_empty() {
            candidates.push(PathBuf::from(p));
        }
    }
    for p in ["/opt/homebrew/bin/codex", "/usr/local/bin/codex", "~/.npm-global/bin/codex", "~/.local/bin/codex", "~/.bun/bin/codex"] {
        candidates.push(expand_home(p));
    }
    candidates.into_iter().find(|p| p.is_file())
}

pub fn expand_home(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h).join(rest);
        }
    }
    PathBuf::from(s)
}

/// Checks the codex install: path, `--version`, `login status`.
pub async fn probe(setting: Option<&str>) -> CodexStatus {
    let setting = setting.map(str::to_string);
    tokio::task::spawn_blocking(move || {
        let Some(path) = resolve_codex(setting.as_deref()) else {
            return CodexStatus { found: false, error: Some("未找到 codex，请先安装 Codex CLI".into()), ..Default::default() };
        };
        let mut st = CodexStatus { found: true, path: Some(path.display().to_string()), ..Default::default() };
        let mut cmd = std::process::Command::new(&path);
        if let Some(p) = login_path() {
            cmd.env("PATH", p);
        }
        match cmd.arg("--version").output() {
            Ok(o) => {
                let v = String::from_utf8_lossy(&o.stdout).trim().to_string();
                st.version = Some(v.strip_prefix("codex-cli ").unwrap_or(&v).to_string());
            }
            Err(e) => st.error = Some(format!("无法运行 codex：{e}")),
        }
        let mut cmd = std::process::Command::new(&path);
        if let Some(p) = login_path() {
            cmd.env("PATH", p);
        }
        if let Ok(o) = cmd.args(["login", "status"]).output() {
            st.logged_in = Some(o.status.success());
        }
        st
    })
    .await
    .unwrap_or_default()
}
