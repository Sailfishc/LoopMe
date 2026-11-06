//! WebSocket server for phones and the Mac app.
//!
//! * `/v1/pair` — pairing. Plain JSON text frames:
//!   phone → `{"t":"pair","code","devicePub","deviceName"}`
//!   mac → `{"t":"pair_pending","sas"}` then `{"t":"paired","deviceId","macName"}` or `{"t":"error","message"}`.
//! * `/v1` — the encrypted session. The first frames are plain text:
//!   phone → `{"t":"hello","deviceId","nonce"}`, mac → `{"t":"hello","nonce"}` (or `{"t":"error",…}`).
//!   Every later frame is binary and sealed (see `crypto`). Inside are JSON
//!   requests `{"id","method","params"}`, responses `{"id","result"|"error"}`
//!   and events `{"event","seq","data"}`.

use crate::app::{App, RunKind};
use crate::crypto::{self, FrameOpener, FrameSealer};
use crate::engine;
use crate::model::*;
use crate::store::DeviceRecord;
use anyhow::{anyhow, bail, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tokio_tungstenite::tungstenite::Message as WsMessage;

pub async fn serve(app: Arc<App>, bind: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(bind).await?;
    tracing::info!("listening on {bind}");
    loop {
        let (stream, peer) = listener.accept().await?;
        let app = app.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_conn(app, stream).await {
                tracing::debug!("connection {peer} closed: {e:#}");
            }
        });
    }
}

type Ws<S> = tokio_tungstenite::WebSocketStream<S>;

/// Bound for the byte streams a session can run over: a direct TCP socket, or
/// a WebSocket the daemon opened to the relay for one phone.
pub trait Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> Io for T {}

/// Runs the pairing (`/v1/pair`) or session (`/v1`) protocol on an open WebSocket.
pub async fn serve_ws<S: Io>(app: Arc<App>, ws: Ws<S>, path: &str) -> Result<()> {
    match path {
        "/v1/pair" => pair(app, ws).await,
        "/v1" => session(app, ws).await,
        _ => Ok(()),
    }
}

async fn handle_conn(app: Arc<App>, stream: TcpStream) -> Result<()> {
    let _ = stream.set_nodelay(true);
    let mut path = String::new();
    let ws = tokio_tungstenite::accept_hdr_async(stream, |req: &Request, resp: Response| {
        path = req.uri().path().to_string();
        Ok(resp)
    })
    .await?;
    serve_ws(app, ws, &path).await
}

async fn recv_text<S: Io>(ws: &mut Ws<S>, timeout: Duration) -> Result<Value> {
    loop {
        let msg = tokio::time::timeout(timeout, ws.next()).await.map_err(|_| anyhow!("timeout"))?;
        match msg {
            Some(Ok(WsMessage::Text(t))) => return Ok(serde_json::from_str(&t)?),
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => continue,
            Some(Ok(_)) => bail!("unexpected frame"),
            Some(Err(e)) => return Err(e.into()),
            None => bail!("closed"),
        }
    }
}

async fn send_text<S: Io>(ws: &mut Ws<S>, v: Value) -> Result<()> {
    ws.send(WsMessage::Text(v.to_string())).await?;
    Ok(())
}

async fn pair<S: Io>(app: Arc<App>, mut ws: Ws<S>) -> Result<()> {
    let hello = recv_text(&mut ws, Duration::from_secs(30)).await?;
    let code = hello.get("code").and_then(Value::as_str).unwrap_or("");
    let device_pub = hello.get("devicePub").and_then(Value::as_str).map(crypto::key32).transpose();
    let name = hello.get("deviceName").and_then(Value::as_str).unwrap_or("iPhone").chars().take(60).collect::<String>();
    let Ok(Some(device_pub)) = device_pub else {
        send_text(&mut ws, json!({"t": "error", "message": "缺少设备公钥"})).await?;
        return Ok(());
    };
    let (sas, rx) = match app.pairing_request(code, &device_pub, &name) {
        Ok(v) => v,
        Err(e) => {
            send_text(&mut ws, json!({"t": "error", "message": e.to_string()})).await?;
            return Ok(());
        }
    };
    send_text(&mut ws, json!({"t": "pair_pending", "sas": sas})).await?;
    let accepted = tokio::select! {
        r = tokio::time::timeout(Duration::from_secs(120), rx) => matches!(r, Ok(Ok(true))),
        // Phone gave up: treat as rejected.
        _ = async { while let Some(Ok(_)) = ws.next().await {} } => false,
    };
    if !accepted {
        app.finish_pairing(None, Some("配对已取消"));
        let _ = send_text(&mut ws, json!({"t": "error", "message": "Mac 上没有确认配对"})).await;
        return Ok(());
    }
    let d = DeviceRecord {
        id: new_id("dev"),
        name,
        public_key: device_pub,
        created_at: now_ms(),
        last_seen_at: Some(now_ms()),
        is_local: false,
        push_token: None,
        push_env: None,
    };
    app.store.save_device(&d)?;
    app.finish_pairing(Some(&d), None);
    send_text(
        &mut ws,
        json!({"t": "paired", "deviceId": d.id, "macName": crate::app::mac_name(), "macPublicKey": crypto::b64(&app.identity.public_bytes())}),
    )
    .await?;
    let _ = ws.close(None).await;
    Ok(())
}

async fn session<S: Io>(app: Arc<App>, mut ws: Ws<S>) -> Result<()> {
    let hello = recv_text(&mut ws, Duration::from_secs(20)).await?;
    let device_id = hello.get("deviceId").and_then(Value::as_str).unwrap_or("").to_string();
    let client_nonce = hello.get("nonce").and_then(Value::as_str).map(crypto::from_b64).transpose()?.unwrap_or_default();
    let Some(device) = app.store.get_device(&device_id)? else {
        send_text(&mut ws, json!({"t": "error", "code": "unknown_device", "message": "这台设备已解除配对"})).await?;
        return Ok(());
    };
    if client_nonce.len() != 16 {
        send_text(&mut ws, json!({"t": "error", "code": "bad_hello", "message": "bad nonce"})).await?;
        return Ok(());
    }
    let server_nonce = crypto::random_bytes::<16>();
    send_text(&mut ws, json!({"t": "hello", "nonce": crypto::b64(&server_nonce)})).await?;
    let shared = app.identity.shared_secret(&device.public_key);
    let keys = crypto::derive_session_keys(&shared, &client_nonce, &server_nonce);
    let mut opener = FrameOpener::new(&keys.c2s);
    let mut sealer = FrameSealer::new(&keys.s2c);
    app.store.touch_device(&device.id)?;

    let (mut sink, mut stream) = ws.split();
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();

    // Writer: seals everything going out (responses and events).
    let writer = tokio::spawn(async move {
        while let Some(v) = out_rx.recv().await {
            let frame = sealer.seal(v.to_string().as_bytes());
            if sink.send(WsMessage::Binary(frame)).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });

    // Event fan-out.
    let mut events = app.events.subscribe();
    let ev_tx = out_tx.clone();
    let event_task = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(e) => {
                    if ev_tx.send(json!(e)).is_err() {
                        break;
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    // Client must resync.
                    let _ = ev_tx.send(json!({"event": "resync", "seq": 0, "data": null}));
                }
                Err(_) => break,
            }
        }
    });

    let mut kicks = app.kicks.subscribe();
    let ctx = Ctx { app: app.clone(), device: device.clone() };
    let result: Result<()> = async {
        loop {
            tokio::select! {
                k = kicks.recv() => {
                    if matches!(k, Ok(ref id) if *id == device.id) {
                        // Tell this authenticated client to forget its credentials before closing.
                        let _ = out_tx.send(json!({"event": "unpaired", "seq": 0, "data": null}));
                        return Ok(());
                    }
                }
                msg = tokio::time::timeout(Duration::from_secs(90), stream.next()) => {
                    let msg = match msg {
                        Err(_) => bail!("idle timeout"),
                        Ok(None) => return Ok(()),
                        Ok(Some(m)) => m?,
                    };
                    match msg {
                        WsMessage::Binary(b) => {
                            let plain = opener.open(&b)?;
                            let req: Value = serde_json::from_slice(&plain)?;
                            let id = req.get("id").cloned().unwrap_or(Value::Null);
                            let method = req.get("method").and_then(Value::as_str).unwrap_or("").to_string();
                            let params = req.get("params").cloned().unwrap_or(json!({}));
                            let ctx = ctx.clone();
                            let tx = out_tx.clone();
                            tokio::spawn(async move {
                                let resp = match handle(&ctx, &method, params).await {
                                    Ok(r) => json!({"id": id, "result": r}),
                                    Err(e) => json!({"id": id, "error": {"code": -1, "message": e.to_string()}}),
                                };
                                let _ = tx.send(resp);
                            });
                        }
                        WsMessage::Close(_) => return Ok(()),
                        _ => {}
                    }
                }
            }
        }
    }
    .await;
    event_task.abort();
    drop(out_tx);
    let _ = writer.await;
    let _ = app.store.touch_device(&device.id);
    result
}

#[derive(Clone)]
struct Ctx {
    app: Arc<App>,
    device: DeviceRecord,
}

fn s<'a>(p: &'a Value, k: &str) -> Result<&'a str> {
    p.get(k).and_then(Value::as_str).ok_or_else(|| anyhow!("缺少参数 {k}"))
}

/// Runs `f` once per client id; replays the stored result for retries.
async fn idempotent<F, Fut>(app: &App, client_id: Option<&str>, f: F) -> Result<Value>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    if let Some(cid) = client_id {
        if let Some(r) = app.store.idempotent_result(cid)? {
            return Ok(r);
        }
    }
    let r = f().await?;
    if let Some(cid) = client_id {
        app.store.record_idempotent(cid, &r)?;
    }
    Ok(r)
}

async fn handle(ctx: &Ctx, method: &str, p: Value) -> Result<Value> {
    let app = &ctx.app;
    let local = ctx.device.is_local;
    match method {
        "ping" => Ok(json!({"pong": now_ms()})),
        "sync" => Ok(json!(app.snapshot()?)),
        "message.send" => {
            let text = s(&p, "text")?.trim().to_string();
            if text.is_empty() {
                bail!("消息为空");
            }
            let cid = p.get("clientId").and_then(Value::as_str);
            let reply_to = p.get("replyTo").and_then(Value::as_str).map(str::to_string);
            idempotent(app, cid, || async {
                let mut m = app.store.insert_message("user", &text, None, None, false)?;
                if let Some(r) = reply_to.as_deref() {
                    if app.store.get_message(r)?.is_none() {
                        bail!("引用的消息不存在");
                    }
                    m = app.store.set_message_extras(&m.id, Some(r), None, None)?;
                }
                app.emit_message(&m);
                app.queue(RunKind::Chat { text: text.clone(), reply_to: reply_to.clone() });
                Ok(json!({"messageId": m.id}))
            })
            .await
        }
        "message.feedback" => {
            let m = app.store.get_message(s(&p, "messageId")?)?.ok_or_else(|| anyhow!("找不到这条消息"))?;
            let value = match p.get("value").and_then(Value::as_str) {
                Some("up") => Some("up"),
                Some("down") => Some("down"),
                None | Some("") => None,
                Some(other) => bail!("未知反馈 {other}"),
            };
            let m = app.store.set_message_extras(&m.id, m.reply_to.as_deref(), value, m.offer.as_ref())?;
            app.emit_message(&m);
            Ok(json!(m))
        }
        "offer.act" => {
            let mid = s(&p, "messageId")?.to_string();
            let action = s(&p, "action")?.to_string();
            let cid = p.get("clientId").and_then(Value::as_str);
            idempotent(app, cid, || async { Ok(json!(engine::offer_act(app, &mid, &action).await?)) }).await
        }
        "offer.diff" => Ok(json!({"diff": engine::offer_diff(app, s(&p, "messageId")?).await?})),
        "app.opened" => {
            // The app came to the foreground: the first time today starts the brief.
            crate::scheduler::maybe_start_brief(app);
            Ok(json!({}))
        }
        "run.stop" => {
            engine::stop(app).await?;
            Ok(json!({}))
        }
        "decision.answer" => {
            let did = s(&p, "decisionId")?.to_string();
            // `later` (回 Mac 再说): no answer from the phone; Codex asks again on the Mac.
            let approved = match s(&p, "choice")? {
                "approve" => Some(true),
                "reject" => Some(false),
                "later" => None,
                other => bail!("未知选择 {other}"),
            };
            let cid = p.get("clientId").and_then(Value::as_str);
            idempotent(app, cid, || async {
                let d = engine::answer_decision(app, &did, approved).await?;
                Ok(json!(d))
            })
            .await
        }
        "item.action" => {
            let i = engine::item_action(app, s(&p, "itemId")?, s(&p, "action")?, p.get("minutes").and_then(Value::as_i64))?;
            Ok(json!(i))
        }
        "memory.add" => {
            let text = s(&p, "text")?.trim().to_string();
            if text.is_empty() {
                bail!("内容为空");
            }
            let now = now_ms();
            let m = MemoryEntry {
                id: new_id("mem"),
                text,
                author: "user".into(),
                status: if p.get("locked").and_then(Value::as_bool).unwrap_or(false) { "locked".into() } else { "active".into() },
                created_at: now,
                updated_at: now,
            };
            app.store.save_memory(&m)?;
            app.emit_memory();
            Ok(json!(m))
        }
        "memory.update" => {
            let mut m = app.store.get_memory(s(&p, "id")?)?.ok_or_else(|| anyhow!("找不到这条记忆"))?;
            if let Some(t) = p.get("text").and_then(Value::as_str) {
                m.text = t.trim().to_string();
                m.author = "user".into(); // a user edit makes it the user's
            }
            if let Some(l) = p.get("locked").and_then(Value::as_bool) {
                if m.status != "deleted" {
                    m.status = if l { "locked".into() } else { "active".into() };
                }
            }
            m.updated_at = now_ms();
            app.store.save_memory(&m)?;
            // The chat thread may hold the old fact; start fresh.
            let _ = app.store.kv_delete("chat_thread");
            app.emit_memory();
            Ok(json!(m))
        }
        "memory.delete" => {
            let mut m = app.store.get_memory(s(&p, "id")?)?.ok_or_else(|| anyhow!("找不到这条记忆"))?;
            m.status = "deleted".into();
            m.updated_at = now_ms();
            app.store.save_memory(&m)?;
            let _ = app.store.kv_delete("chat_thread");
            app.emit_memory();
            Ok(json!(m))
        }
        "memory.restore" => {
            let mut m = app.store.get_memory(s(&p, "id")?)?.ok_or_else(|| anyhow!("找不到这条记忆"))?;
            m.status = "active".into();
            m.updated_at = now_ms();
            app.store.save_memory(&m)?;
            app.emit_memory();
            Ok(json!(m))
        }
        "profile.update" => {
            let mut cur = serde_json::to_value(app.store.profile()?)?;
            merge(&mut cur, &p);
            let profile: Profile = serde_json::from_value(cur)?;
            app.store.set_json("profile", &profile)?;
            let _ = app.store.kv_delete("chat_thread");
            app.emit("profile", json!(profile));
            Ok(json!(profile))
        }
        "settings.update" => {
            let before = app.store.settings()?;
            let mut cur = serde_json::to_value(&before)?;
            let codex_before = cur.get("codexPath").cloned();
            merge(&mut cur, &p);
            let mut settings: Settings = serde_json::from_value(cur)?;
            if !["readOnly", "workspace", "full"].contains(&settings.permission_level.as_str()) {
                bail!("未知权限档位");
            }
            if !local && (p.get("codexPath").is_some() || p.get("workspaceRoots").is_some() || p.get("ignoredDirs").is_some() || p.get("relayHosts").is_some()) {
                bail!("这一项只能在 Mac 上修改");
            }
            if !settings.paused {
                settings.paused_until = None;
            }
            // Only hooks.enable / hooks.disable change this (they install or remove the hooks).
            settings.codex_hooks = before.codex_hooks;
            app.store.set_json("settings", &settings)?;
            app.emit("settings", json!(settings));
            if p.get("relayHosts").is_some() {
                app.status.lock().unwrap().hosts = app.phone_hosts();
            }
            app.emit_status();
            if p.get("codexPath").is_some() && p.get("codexPath") != codex_before.as_ref() {
                *app.engine.codex.lock().await = None;
                let a = app.clone();
                tokio::spawn(async move { a.refresh_codex_status().await });
            }
            Ok(json!(settings))
        }
        "push.register" => {
            let token = s(&p, "token")?.to_string();
            let env = p.get("env").and_then(Value::as_str).unwrap_or("sandbox").to_string();
            let mut d = app.store.get_device(&ctx.device.id)?.ok_or_else(|| anyhow!("device gone"))?;
            d.push_token = Some(token);
            d.push_env = Some(if env == "production" { env } else { "sandbox".into() });
            app.store.save_device(&d)?;
            app.emit_devices();
            Ok(json!({}))
        }
        "push.test" => Ok(json!({"report": crate::notify::test_push(app).await?})),
        "session.ack" => Ok(json!(crate::attention::ack(app, s(&p, "sessionId")?)?)),
        "session.snooze" => {
            let minutes = p.get("minutes").and_then(Value::as_i64);
            Ok(json!(crate::attention::snooze(app, s(&p, "sessionId")?, minutes)?))
        }
        "session.mute" => {
            let id = s(&p, "sessionId")?;
            let mut sess = app.store.get_session(id)?.ok_or_else(|| anyhow!("找不到这个会话"))?;
            sess.muted = true;
            sess.eval_at = None;
            sess.attention = None;
            app.store.save_session(&sess)?;
            app.emit("session", json!(sess));
            Ok(json!(sess))
        }
        "sessions.analyze" => {
            app.queue(RunKind::Brief { manual: true });
            Ok(json!({}))
        }
        "device.unpair" => {
            // A phone removing itself.
            if !local {
                app.revoke_device(&ctx.device.id)?;
            }
            Ok(json!({}))
        }
        // ---- Mac app only ----
        "pairing.start" | "pairing.cancel" | "pairing.confirm" | "pairing.state" | "devices.revoke" | "status.refresh" | "codex.login" | "brief.now"
        | "hooks.enable" | "hooks.disable"
            if !local =>
        {
            bail!("只能在 Mac 上操作")
        }
        "pairing.start" => Ok(json!(app.start_pairing())),
        "pairing.cancel" => {
            app.cancel_pairing();
            Ok(json!({}))
        }
        "pairing.state" => Ok(json!(app.pairing_state())),
        "pairing.confirm" => {
            app.confirm_pairing(p.get("accept").and_then(Value::as_bool).unwrap_or(false))?;
            Ok(json!({}))
        }
        "devices.revoke" => {
            app.revoke_device(s(&p, "deviceId")?)?;
            Ok(json!({}))
        }
        "status.refresh" => {
            app.refresh_codex_status().await;
            Ok(json!(app.status()))
        }
        "codex.login" => {
            let settings = app.store.settings()?;
            let path = crate::codex::resolve_codex(settings.codex_path.as_deref()).ok_or_else(|| anyhow!("未找到 codex"))?;
            open_terminal(&format!("'{}' login", path.display()))?;
            Ok(json!({}))
        }
        "brief.now" => {
            app.queue(RunKind::Brief { manual: true });
            Ok(json!({}))
        }
        "hooks.enable" => {
            // The one-time confirm: install LoopMe's hooks and mark them trusted in Codex.
            let state = crate::scheduler::enable_hooks(app).await?;
            Ok(json!({"hooks": state}))
        }
        "hooks.disable" => {
            crate::hooks::uninstall()?;
            let mut settings = app.store.settings()?;
            settings.codex_hooks = false;
            app.store.set_json("settings", &settings)?;
            app.emit("settings", json!(settings));
            app.set_hooks_status("off");
            Ok(json!({"hooks": "off"}))
        }
        other => bail!("未知方法 {other}"),
    }
}

/// Shallow-merges `patch` into `base`; nested objects merge one level deep.
fn merge(base: &mut Value, patch: &Value) {
    let (Some(b), Some(p)) = (base.as_object_mut(), patch.as_object()) else { return };
    for (k, v) in p {
        match (b.get_mut(k), v) {
            (Some(Value::Object(bo)), Value::Object(po)) => {
                for (k2, v2) in po {
                    bo.insert(k2.clone(), v2.clone());
                }
            }
            _ => {
                b.insert(k.clone(), v.clone());
            }
        }
    }
}

fn open_terminal(cmd: &str) -> Result<()> {
    let script = format!(
        "tell application \"Terminal\"\n activate\n do script \"{}\"\nend tell",
        cmd.replace('\\', "\\\\").replace('"', "\\\"")
    );
    std::process::Command::new("osascript").args(["-e", &script]).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_is_one_level_deep() {
        let mut a = json!({"a": 1, "n": {"x": 1, "y": 2}});
        merge(&mut a, &json!({"a": 2, "n": {"y": 3}}));
        assert_eq!(a, json!({"a": 2, "n": {"x": 1, "y": 3}}));
    }
}
