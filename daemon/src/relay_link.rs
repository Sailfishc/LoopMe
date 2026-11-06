//! The daemon's side of the relay: it keeps one outbound control connection
//! to the relay and opens a fresh outbound WebSocket for every phone that
//! connects, then runs the usual pairing / session protocol over it
//! (`server::serve_ws`). The relay only ever forwards ciphertext.
//!
//! By default the relay runs next to the daemon (`loopme-daemon relay`,
//! started and restarted by `supervise_local`) and the daemon reaches it on a
//! loopback port. Pointing `--relay-url` somewhere else moves the relay to
//! another machine without changing anything on the phone side except the
//! addresses in the QR code.

use crate::app::App;
use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as WsMessage;

#[derive(Clone, Debug)]
pub struct LinkConfig {
    /// Where the Mac side of the relay is, e.g. `ws://127.0.0.1:8444`.
    pub base: String,
    /// Needed when the relay is not on this Mac.
    pub mac_token: Option<String>,
    /// The port phones use (shown in the QR code).
    pub public_port: u16,
    /// Hosts phones should use instead of this Mac's addresses (remote relay).
    pub public_hosts: Vec<String>,
}

/// Stable id the relay routes by (not secret).
pub fn mac_id(app: &App) -> String {
    let h = Sha256::digest(app.identity.public_bytes());
    crate::crypto::b64url(&h[..12])
}

/// Bearer token phones present to the relay. Rotating it (re-pairing) locks out old phones at the relay.
pub fn phone_token(app: &App) -> String {
    if let Ok(Some(t)) = app.store.kv_get("relay_phone_token") {
        return t;
    }
    let t = crate::crypto::b64url(&crate::crypto::random_bytes::<18>());
    let _ = app.store.kv_set("relay_phone_token", &t);
    t
}

pub async fn run(app: Arc<App>, cfg: LinkConfig) {
    let mut delay = 250u64;
    let mut failures = 0;
    loop {
        if failures == 0 {
            app.set_relay_status("connecting");
        }
        match control(&app, &cfg).await {
            Ok(()) => {
                delay = 250;
                failures = 0;
            }
            Err(e) => {
                failures += 1;
                // The co-located relay may still be starting; only report after a few tries.
                if failures >= 4 {
                    tracing::warn!("relay link: {e:#}");
                    app.set_relay_status("error");
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(delay)).await;
        delay = (delay * 2).min(30_000);
    }
}

async fn control(app: &Arc<App>, cfg: &LinkConfig) -> Result<()> {
    let (mut ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(format!("{}/v1/mac", cfg.base)))
        .await
        .map_err(|_| anyhow!("connect timeout"))??;
    let mut reg = json!({"t": "register", "macId": mac_id(app), "phoneToken": phone_token(app)});
    if let Some(t) = &cfg.mac_token {
        reg["macToken"] = json!(t);
    }
    ws.send(WsMessage::Text(reg.to_string())).await?;
    match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
        Ok(Some(Ok(WsMessage::Text(t)))) => {
            let v: Value = serde_json::from_str(&t)?;
            if v.get("t").and_then(Value::as_str) != Some("registered") {
                bail!("relay refused: {}", v.get("message").and_then(Value::as_str).unwrap_or("?"));
            }
        }
        _ => bail!("no answer to register"),
    }
    tracing::info!("connected to relay {}", cfg.base);
    app.set_relay_status("on");
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            _ = ping.tick() => ws.send(WsMessage::Ping(vec![])).await?,
            m = tokio::time::timeout(Duration::from_secs(70), ws.next()) => {
                let m = match m {
                    Err(_) => bail!("relay idle"),
                    Ok(None) => return Ok(()),
                    Ok(Some(m)) => m?,
                };
                match m {
                    WsMessage::Text(t) => {
                        let v: Value = serde_json::from_str(&t)?;
                        if v.get("t").and_then(Value::as_str) == Some("incoming") {
                            let conn = v.get("conn").and_then(Value::as_str).unwrap_or("").to_string();
                            let path = v.get("path").and_then(Value::as_str).unwrap_or("").to_string();
                            let app = app.clone();
                            let base = cfg.base.clone();
                            tokio::spawn(async move {
                                if let Err(e) = accept(app, &base, &conn, &path).await {
                                    tracing::debug!("relayed connection: {e:#}");
                                }
                            });
                        }
                    }
                    WsMessage::Close(_) => return Ok(()),
                    _ => {}
                }
            }
        }
    }
}

async fn accept(app: Arc<App>, base: &str, conn: &str, path: &str) -> Result<()> {
    let url = format!("{base}/v1/mac/accept?conn={conn}");
    let (ws, _) = tokio::time::timeout(Duration::from_secs(10), tokio_tungstenite::connect_async(url)).await.map_err(|_| anyhow!("accept timeout"))??;
    crate::server::serve_ws(app, ws, path).await
}

/// Runs `loopme-daemon relay` next to the daemon and restarts it if it exits.
pub async fn supervise_local(data_dir: PathBuf, public_port: u16, mac_port: u16) {
    let Ok(exe) = std::env::current_exe() else {
        tracing::error!("relay: can't find own executable");
        return;
    };
    let mut delay = 1u64;
    loop {
        let started = std::time::Instant::now();
        let child = tokio::process::Command::new(&exe)
            .arg("relay")
            .args(["--data-dir", &data_dir.display().to_string()])
            .args(["--listen", &format!("0.0.0.0:{public_port}")])
            .args(["--mac-listen", &format!("127.0.0.1:{mac_port}")])
            .arg("--exit-with-parent")
            // The relay exits when this pipe closes, so it never outlives the daemon (even after SIGKILL).
            .stdin(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn();
        match child {
            Ok(mut c) => {
                let _keep_open = c.stdin.take();
                let st = c.wait().await;
                tracing::warn!("relay exited: {st:?}");
            }
            Err(e) => tracing::error!("relay failed to start: {e}"),
        }
        if started.elapsed() > Duration::from_secs(60) {
            delay = 1;
        }
        tokio::time::sleep(Duration::from_secs(delay)).await;
        delay = (delay * 2).min(60);
    }
}
