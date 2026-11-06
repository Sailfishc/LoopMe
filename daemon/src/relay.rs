//! `loopme-daemon relay`: a thin forwarding relay for phones on the company VPN.
//!
//! The relay never sees plaintext: it splices a phone's WebSocket to a
//! WebSocket the Mac opens for it, frame by frame. Pairing, end-to-end
//! encryption and the business protocol stay between the phone and the daemon.
//!
//! Endpoints (one public port, WSS with a self-signed certificate whose
//! SHA-256 fingerprint is pinned through the pairing QR code):
//! * `GET /healthz[?mac=<id>&t=<phone token>]` — plain HTTP, for diagnostics.
//! * `/v1/mac` — a Mac registers: `{"t":"register","macId","macToken"?,"phoneToken"}`
//!   → `{"t":"registered"}`; later the relay sends `{"t":"incoming","conn","path"}`.
//! * `/v1/mac/accept?conn=<id>` — the Mac's side of one phone connection.
//! * `/v1/phone/<macId>/pair|session?t=<phone token>` — a phone; spliced to the Mac.
//!
//! `/v1/mac` on the loopback-only `--mac-listen` port needs no token (the relay
//! runs next to the daemon); on the public port it needs one of `LOOPME_RELAY_MAC_TOKENS`.
//! Nothing is written to disk except the relay's own certificate.

use anyhow::{anyhow, bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskCx, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::WebSocketStream;

pub const DEFAULT_PORT: u16 = 8443;

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

/// A stream with some bytes already read off the front (the HTTP head we peeked at).
pub struct Prefixed {
    head: Vec<u8>,
    pos: usize,
    inner: Box<dyn Io>,
}

impl AsyncRead for Prefixed {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut TaskCx<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.pos < self.head.len() {
            let n = (self.head.len() - self.pos).min(buf.remaining());
            let start = self.pos;
            buf.put_slice(&self.head[start..start + n]);
            self.pos += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Prefixed {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut TaskCx<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskCx<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskCx<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

type Ws = WebSocketStream<Prefixed>;

pub struct Config {
    pub listen: SocketAddr,
    /// Loopback listener for the co-located daemon (no token needed).
    pub mac_listen: Option<SocketAddr>,
    pub tls: bool,
    pub dir: PathBuf,
    /// Tokens accepted for `/v1/mac` on the public port.
    pub mac_tokens: Vec<String>,
    /// Exit when stdin closes (the daemon that started us is gone).
    pub exit_with_parent: bool,
}

struct MacEntry {
    gen: u64,
    phone_token: String,
    ctl: mpsc::UnboundedSender<Value>,
}

#[derive(Default)]
struct State {
    macs: Mutex<HashMap<String, MacEntry>>,
    pending: Mutex<HashMap<String, oneshot::Sender<Ws>>>,
    gen: std::sync::atomic::AtomicU64,
}

// ---- certificate ----

pub fn cert_path(dir: &Path) -> PathBuf {
    dir.join("relay").join("cert.der")
}

/// SHA-256 of the relay certificate (lowercase hex), as pinned by phones.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn fingerprint_file(dir: &Path) -> Option<String> {
    std::fs::read(cert_path(dir)).ok().map(|d| fingerprint(&d))
}

/// Loads or creates the self-signed certificate (`<dir>/relay/cert.der`, `key.der`).
pub fn ensure_cert(dir: &Path) -> Result<(Vec<u8>, Vec<u8>)> {
    let cert = cert_path(dir);
    let key = dir.join("relay").join("key.der");
    if let (Ok(c), Ok(k)) = (std::fs::read(&cert), std::fs::read(&key)) {
        return Ok((c, k));
    }
    std::fs::create_dir_all(cert.parent().unwrap())?;
    let ck = rcgen::generate_simple_self_signed(vec!["loopme-relay".to_string()])?;
    let c = ck.cert.der().to_vec();
    let k = ck.key_pair.serialize_der();
    crate::app::write_private(&key, &k)?;
    std::fs::write(&cert, &c)?;
    Ok((c, k))
}

fn tls_acceptor(dir: &Path) -> Result<tokio_rustls::TlsAcceptor> {
    let (c, k) = ensure_cert(dir)?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(c)],
            rustls::pki_types::PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(k)),
        )?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(cfg)))
}

// ---- entry point ----

pub fn parse_args(args: &[String], default_dir: PathBuf) -> Result<Config> {
    let mut cfg = Config {
        listen: format!("0.0.0.0:{DEFAULT_PORT}").parse()?,
        mac_listen: None,
        tls: true,
        dir: default_dir,
        mac_tokens: std::env::var("LOOPME_RELAY_MAC_TOKENS")
            .ok()
            .map(|s| s.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
            .unwrap_or_default(),
        exit_with_parent: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().ok_or_else(|| anyhow!("{a} needs a value"));
        match a.as_str() {
            "--listen" => cfg.listen = val()?.parse().context("bad --listen")?,
            "--mac-listen" => cfg.mac_listen = Some(val()?.parse().context("bad --mac-listen")?),
            "--data-dir" => cfg.dir = PathBuf::from(val()?),
            "--no-tls" => cfg.tls = false,
            "--exit-with-parent" => cfg.exit_with_parent = true,
            other => bail!("unknown relay argument {other}"),
        }
    }
    Ok(cfg)
}

pub async fn run(cfg: Config) -> Result<()> {
    if cfg.exit_with_parent {
        tokio::spawn(async {
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 64];
            while matches!(stdin.read(&mut buf).await, Ok(n) if n > 0) {}
            tracing::info!("daemon is gone; relay exiting");
            std::process::exit(0);
        });
    }
    let state = Arc::new(State::default());
    let tls = if cfg.tls { Some(tls_acceptor(&cfg.dir)?) } else { None };
    if let Some(fp) = fingerprint_file(&cfg.dir).filter(|_| cfg.tls) {
        tracing::info!("relay certificate sha256 {fp}");
    }
    let tokens = Arc::new(cfg.mac_tokens.clone());
    let public = TcpListener::bind(cfg.listen).await.with_context(|| format!("relay can't listen on {}", cfg.listen))?;
    tracing::info!("relay listening on {} ({})", cfg.listen, if cfg.tls { "wss" } else { "ws" });
    if let Some(addr) = cfg.mac_listen {
        anyhow::ensure!(addr.ip().is_loopback(), "--mac-listen must be a loopback address");
        let l = TcpListener::bind(addr).await.with_context(|| format!("relay can't listen on {addr}"))?;
        tracing::info!("relay mac side on {addr}");
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = l.accept().await else { continue };
                let st = st.clone();
                tokio::spawn(async move {
                    let _ = tcp.set_nodelay(true);
                    if let Err(e) = handle(st, Box::new(tcp), true, Arc::new(vec![])).await {
                        tracing::debug!("relay mac conn: {e:#}");
                    }
                });
            }
        });
    }
    loop {
        let (tcp, peer) = public.accept().await?;
        let st = state.clone();
        let tls = tls.clone();
        let tokens = tokens.clone();
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            let io: Result<Box<dyn Io>> = match tls {
                Some(acc) => match tokio::time::timeout(Duration::from_secs(10), acc.accept(tcp)).await {
                    Ok(Ok(s)) => Ok(Box::new(s)),
                    Ok(Err(e)) => Err(e.into()),
                    Err(_) => Err(anyhow!("tls timeout")),
                },
                None => Ok(Box::new(tcp)),
            };
            let r = match io {
                Ok(io) => handle(st, io, false, tokens).await,
                Err(e) => Err(e),
            };
            if let Err(e) = r {
                tracing::debug!("relay conn {peer}: {e:#}");
            }
        });
    }
}

// ---- per connection ----

struct Head {
    path: String,
    query: HashMap<String, String>,
    upgrade: bool,
}

fn parse_head(raw: &[u8]) -> Result<Head> {
    let text = String::from_utf8_lossy(raw);
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let target = first.split_whitespace().nth(1).ok_or_else(|| anyhow!("bad request line"))?;
    let (path, q) = target.split_once('?').unwrap_or((target, ""));
    let query = q
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), percent_decode(v)))
        .collect();
    let upgrade = lines.any(|l| {
        let l = l.to_ascii_lowercase();
        l.starts_with("upgrade:") && l.contains("websocket")
    });
    Ok(Head { path: path.to_string(), query, upgrade })
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn read_head(io: &mut Box<dyn Io>) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(10), io.read(&mut chunk)).await.map_err(|_| anyhow!("head timeout"))??;
        if n == 0 {
            bail!("closed before request");
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            return Ok(buf);
        }
        if buf.len() > 16 * 1024 {
            bail!("request head too large");
        }
    }
}

fn eq_ct(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn handle(st: Arc<State>, mut io: Box<dyn Io>, mac_side: bool, tokens: Arc<Vec<String>>) -> Result<()> {
    let raw = read_head(&mut io).await?;
    let head = parse_head(&raw)?;
    if !head.upgrade {
        let (code, body) = if head.path == "/healthz" {
            let mac = head.query.get("mac").cloned().unwrap_or_default();
            let token = head.query.get("t").cloned().unwrap_or_default();
            let online = {
                let macs = st.macs.lock().unwrap();
                macs.get(&mac).map(|m| eq_ct(&m.phone_token, &token))
            };
            let mut v = json!({"ok": true, "service": "loopme-relay", "version": crate::app::VERSION});
            if !mac.is_empty() {
                // Only a phone holding the token learns whether its Mac is online.
                v["macOnline"] = json!(online == Some(true));
            }
            ("200 OK", v.to_string())
        } else {
            ("404 Not Found", json!({"ok": false}).to_string())
        };
        let resp = format!(
            "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        io.write_all(resp.as_bytes()).await?;
        let _ = io.shutdown().await;
        return Ok(());
    }
    let stream = Prefixed { head: raw, pos: 0, inner: io };
    let cfg = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(64 << 20),
        max_frame_size: Some(64 << 20),
        ..Default::default()
    };
    let ws = tokio_tungstenite::accept_async_with_config(stream, Some(cfg)).await?;
    let path = head.path.clone();
    if path == "/v1/mac" {
        return mac_control(st, ws, mac_side, &tokens).await;
    }
    if path == "/v1/mac/accept" {
        let conn = head.query.get("conn").cloned().unwrap_or_default();
        let tx = st.pending.lock().unwrap().remove(&conn);
        match tx {
            Some(tx) => {
                let _ = tx.send(ws);
            }
            None => {
                let mut ws = ws;
                let _ = ws.close(None).await;
            }
        }
        return Ok(());
    }
    if let Some(rest) = path.strip_prefix("/v1/phone/") {
        let (mac_id, kind) = rest.split_once('/').ok_or_else(|| anyhow!("bad phone path"))?;
        let target = match kind {
            "pair" => "/v1/pair",
            "session" => "/v1",
            _ => bail!("bad phone path"),
        };
        let token = head.query.get("t").cloned().unwrap_or_default();
        return phone(st, ws, mac_id.to_string(), token, target).await;
    }
    let mut ws = ws;
    let _ = ws.close(None).await;
    Ok(())
}

async fn reject(mut ws: Ws, code: &str, message: &str) -> Result<()> {
    let _ = ws.send(WsMessage::Text(json!({"t": "error", "code": code, "message": message}).to_string())).await;
    let _ = ws.close(None).await;
    Ok(())
}

async fn mac_control(st: Arc<State>, mut ws: Ws, mac_side: bool, tokens: &[String]) -> Result<()> {
    let first = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.map_err(|_| anyhow!("register timeout"))?;
    let Some(Ok(WsMessage::Text(t))) = first else { bail!("expected register") };
    let v: Value = serde_json::from_str(&t)?;
    let mac_id = v.get("macId").and_then(Value::as_str).unwrap_or("").to_string();
    let phone_token = v.get("phoneToken").and_then(Value::as_str).unwrap_or("").to_string();
    let mac_token = v.get("macToken").and_then(Value::as_str).unwrap_or("");
    if mac_id.is_empty() || phone_token.len() < 16 {
        return reject(ws, "bad_register", "macId / phoneToken missing").await;
    }
    if !mac_side && !tokens.iter().any(|t| eq_ct(t, mac_token)) {
        return reject(ws, "bad_token", "mac token rejected").await;
    }
    let gen = st.gen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let (ctl_tx, mut ctl_rx) = mpsc::unbounded_channel::<Value>();
    st.macs.lock().unwrap().insert(mac_id.clone(), MacEntry { gen, phone_token, ctl: ctl_tx });
    tracing::info!("mac {mac_id} registered");
    ws.send(WsMessage::Text(json!({"t": "registered"}).to_string())).await?;
    let mut ping = tokio::time::interval(Duration::from_secs(20));
    let result: Result<()> = async {
        loop {
            tokio::select! {
                m = ctl_rx.recv() => match m {
                    Some(v) => ws.send(WsMessage::Text(v.to_string())).await?,
                    None => return Ok(()), // replaced by a newer registration
                },
                _ = ping.tick() => ws.send(WsMessage::Ping(vec![])).await?,
                m = tokio::time::timeout(Duration::from_secs(70), ws.next()) => match m {
                    Err(_) => bail!("mac idle"),
                    Ok(None) | Ok(Some(Ok(WsMessage::Close(_)))) => return Ok(()),
                    Ok(Some(Err(e))) => return Err(e.into()),
                    Ok(Some(Ok(_))) => {}
                },
            }
        }
    }
    .await;
    let mut macs = st.macs.lock().unwrap();
    if macs.get(&mac_id).is_some_and(|m| m.gen == gen) {
        macs.remove(&mac_id);
        tracing::info!("mac {mac_id} went offline");
    }
    result
}

async fn phone(st: Arc<State>, ws: Ws, mac_id: String, token: String, target: &str) -> Result<()> {
    // Decide under the lock, answer after it (a std mutex guard must not live across an await).
    let found: Result<mpsc::UnboundedSender<Value>, (&str, &str)> = {
        let macs = st.macs.lock().unwrap();
        match macs.get(&mac_id) {
            None => Err(("mac_offline", "Relay 在线，但 Mac 上的 LoopMe 没有连上 Relay")),
            Some(m) if !eq_ct(&m.phone_token, &token) => Err(("bad_token", "这台手机的连接凭据已失效，请重新扫码配对")),
            Some(m) => Ok(m.ctl.clone()),
        }
    };
    let ctl = match found {
        Ok(c) => c,
        Err((code, msg)) => return reject(ws, code, msg).await,
    };
    let conn = crate::crypto::b64url(&crate::crypto::random_bytes::<16>());
    let (tx, rx) = oneshot::channel();
    st.pending.lock().unwrap().insert(conn.clone(), tx);
    let _ = ctl.send(json!({"t": "incoming", "conn": conn, "path": target}));
    let mac_ws = match tokio::time::timeout(Duration::from_secs(10), rx).await {
        Ok(Ok(w)) => w,
        _ => {
            st.pending.lock().unwrap().remove(&conn);
            return reject(ws, "mac_offline", "Mac 没有响应").await;
        }
    };
    splice(ws, mac_ws).await;
    Ok(())
}

/// Forwards frames both ways until either side closes.
pub async fn splice<A, B>(a: WebSocketStream<A>, b: WebSocketStream<B>)
where
    A: AsyncRead + AsyncWrite + Unpin + Send,
    B: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (mut a_tx, mut a_rx) = a.split();
    let (mut b_tx, mut b_rx) = b.split();
    let a_to_b = async {
        while let Some(Ok(m)) = a_rx.next().await {
            if matches!(m, WsMessage::Close(_)) || b_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = b_tx.close().await;
    };
    let b_to_a = async {
        while let Some(Ok(m)) = b_rx.next().await {
            if matches!(m, WsMessage::Close(_)) || a_tx.send(m).await.is_err() {
                break;
            }
        }
        let _ = a_tx.close().await;
    };
    tokio::select! {
        _ = a_to_b => {}
        _ = b_to_a => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heads_parse() {
        let h = parse_head(b"GET /v1/phone/abc/session?t=x%2By HTTP/1.1\r\nHost: a\r\nUpgrade: websocket\r\n\r\n").unwrap();
        assert_eq!(h.path, "/v1/phone/abc/session");
        assert_eq!(h.query.get("t").unwrap(), "x+y");
        assert!(h.upgrade);
        let h = parse_head(b"GET /healthz HTTP/1.1\r\n\r\n").unwrap();
        assert!(!h.upgrade);
    }

    #[test]
    fn cert_is_stable() {
        let tmp = tempfile::tempdir().unwrap();
        let (a, _) = ensure_cert(tmp.path()).unwrap();
        let (b, _) = ensure_cert(tmp.path()).unwrap();
        assert_eq!(a, b);
        assert_eq!(fingerprint_file(tmp.path()).unwrap().len(), 64);
    }
}
