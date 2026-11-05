use anyhow::{Context, Result};
use loopme::app::App;
use std::net::SocketAddr;
use std::path::PathBuf;

const USAGE: &str = "loopme-daemon [--data-dir DIR] [--port 7788] [--bind 0.0.0.0]

  --data-dir   where the database, keys and local-client.json live
               (default: ~/Library/Application Support/LoopMe)
  --port       WebSocket port for phones and the Mac app (default 7788)
  --bind       listen address (default 0.0.0.0; use 127.0.0.1 to disable phones)

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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let mut data_dir = default_data_dir();
    let mut port: u16 = 7788;
    let mut bind = "0.0.0.0".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--data-dir" => data_dir = PathBuf::from(args.next().context("--data-dir needs a value")?),
            "--port" => port = args.next().context("--port needs a value")?.parse()?,
            "--bind" => bind = args.next().context("--bind needs a value")?,
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
    tracing::info!("data dir {}", data_dir.display());

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
    Ok(())
}
