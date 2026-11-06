use anyhow::{Context, Result};
use loopme::app::App;
use std::net::SocketAddr;
use std::path::PathBuf;

const USAGE: &str = "loopme-daemon [--data-dir DIR] [--port 7788] [--bind ADDR] [--relay-port 8443 | --relay-url URL]
loopme-daemon relay [--listen 0.0.0.0:8443] [--mac-listen 127.0.0.1:8444] [--no-tls] [--data-dir DIR]

  --data-dir    where the database, keys and local-client.json live
                (default: ~/Library/Application Support/LoopMe)
  --port        WebSocket port for the Mac app (default 7788)
  --bind        listen address of that port (default 127.0.0.1 with a relay,
                0.0.0.0 with --relay-port 0 so phones connect directly)
  --relay-port  phones connect to a relay started next to the daemon on this
                port (WSS, default 8443; 0 = no relay); its Mac side is the next port
  --relay-url   use a relay elsewhere instead, e.g. ws://relay.corp:8444
                (with LOOPME_RELAY_MAC_TOKEN and --relay-hosts host,host for the QR code)

Environment: LOOPME_CODEX (codex binary), LOOPME_HOSTS (addresses for the QR code),
RUST_LOG (log filter, default info).";

fn default_data_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/LoopMe")
    } else {
        PathBuf::from(home).join(".loopme")
    }
}

mod url {
    /// The port in `ws://host:port/...` (only used for the remote relay's public port default).
    pub fn port_of(url: &str) -> Option<u16> {
        let rest = url.split_once("://")?.1;
        let host = rest.split('/').next()?;
        host.rsplit_once(':')?.1.parse().ok()
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // `loopme-daemon hook --loopme-data-dir DIR`: run by Codex as a hook (see hooks.rs).
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("hook") {
        let dir = argv.iter().position(|a| a == "--loopme-data-dir").and_then(|i| argv.get(i + 1)).map(PathBuf::from).unwrap_or_else(default_data_dir);
        loopme::hooks::hook_main(&dir);
        return Ok(());
    }

    if argv.get(1).map(String::as_str) == Some("relay") {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
            .init();
        let cfg = loopme::relay::parse_args(&argv[2..], default_data_dir())?;
        return loopme::relay::run(cfg).await;
    }

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mut data_dir = default_data_dir();
    let mut port: u16 = 7788;
    let mut bind: Option<String> = None;
    let mut relay_port: u16 = loopme::relay::DEFAULT_PORT;
    let mut relay_url: Option<String> = None;
    let mut relay_hosts: Vec<String> = vec![];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => data_dir = PathBuf::from(args.next().context("--data-dir needs a value")?),
            "--port" => port = args.next().context("--port needs a value")?.parse()?,
            "--bind" => bind = Some(args.next().context("--bind needs a value")?),
            "--relay-port" => relay_port = args.next().context("--relay-port needs a value")?.parse()?,
            "--relay-url" => relay_url = Some(args.next().context("--relay-url needs a value")?),
            "--relay-hosts" => {
                relay_hosts = args.next().context("--relay-hosts needs a value")?.split(',').map(|h| h.trim().to_string()).filter(|h| !h.is_empty()).collect()
            }
            "--version" | "-V" => {
                println!("loopme-daemon {}", loopme::app::VERSION);
                return Ok(());
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}\n\n{USAGE}"),
        }
    }

    let (app, runs) = App::new(&data_dir, port)?;
    app.ensure_local_client()?;
    let pid_file = data_dir.join("daemon.pid");
    std::fs::write(&pid_file, std::process::id().to_string())?;
    // The Mac app passes its build number, so after an update it can tell an old daemon from its own.
    let build_file = data_dir.join("daemon.build");
    match std::env::var("LOOPME_APP_BUILD") {
        Ok(b) if !b.is_empty() => std::fs::write(&build_file, b)?,
        _ => {
            let _ = std::fs::remove_file(&build_file);
        }
    }
    tracing::info!("data dir {}", data_dir.display());

    // Phones reach the Mac through a relay; the daemon itself only listens locally.
    let relay = match (&relay_url, relay_port) {
        (Some(url), _) => {
            let public_port = url::port_of(url).unwrap_or(loopme::relay::DEFAULT_PORT);
            Some(loopme::relay_link::LinkConfig {
                base: url.trim_end_matches('/').to_string(),
                mac_token: std::env::var("LOOPME_RELAY_MAC_TOKEN").ok(),
                public_port,
                public_hosts: relay_hosts.clone(),
            })
        }
        (None, 0) => None,
        (None, p) => {
            let mac_port = p.checked_add(1).context("--relay-port too large")?;
            loopme::relay::ensure_cert(&data_dir)?;
            tokio::spawn(loopme::relay_link::supervise_local(data_dir.clone(), p, mac_port));
            Some(loopme::relay_link::LinkConfig {
                base: format!("ws://127.0.0.1:{mac_port}"),
                mac_token: None,
                public_port: p,
                public_hosts: vec![],
            })
        }
    };
    let bind = bind.unwrap_or_else(|| if relay.is_some() { "127.0.0.1".into() } else { "0.0.0.0".into() });
    if let Some(cfg) = relay {
        app.use_relay(cfg.clone());
        tokio::spawn(loopme::relay_link::run(app.clone(), cfg));
    }
    app.status.lock().unwrap().hosts = app.phone_hosts();

    tokio::spawn(loopme::engine::run_worker(app.clone(), runs));
    tokio::spawn(loopme::scheduler::run(app.clone()));

    let addr: SocketAddr = format!("{bind}:{port}").parse().context("bad --bind/--port")?;
    let server = tokio::spawn(loopme::server::serve(app.clone(), addr));

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    tokio::select! {
        r = server => { r??; }
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    tracing::info!("shutting down");
    // Dropping the codex client kills the app-server child.
    *app.engine.codex.lock().await = None;
    let _ = std::fs::remove_file(&pid_file);
    let _ = std::fs::remove_file(&build_file);
    Ok(())
}
