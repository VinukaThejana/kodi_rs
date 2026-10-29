//! A web remote for Kodi: an Axum server rendering an htmx UI that drives Kodi over JSON-RPC.
//!
//! Configuration (environment):
//! - `KODI_PASSWORD` (required) — Kodi's web server password
//! - `KODI_USER` — defaults to `kodi`
//! - `KODI_URL` — defaults to `http://127.0.0.1:8080`
//! - `BIND` — defaults to `0.0.0.0:8090`
//! - `GO2RTC_URL` — go2rtc's API for "Listen on this device"; defaults to `http://127.0.0.1:1984`

mod kodi;
mod listen;
mod routes;
mod views;

use std::{env, io::IsTerminal, path::PathBuf};

use kodi::Kodi;
use listen::Listen;
use routes::AppState;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    // Plain output under systemd, so the journal doesn't fill with colour escape codes.
    tracing_subscriber::fmt().with_ansi(std::io::stdout().is_terminal()).init();

    let var = |key: &str, default: &str| env::var(key).unwrap_or_else(|_| default.to_owned());
    let Ok(password) = env::var("KODI_PASSWORD") else {
        eprintln!("KODI_PASSWORD must be set");
        std::process::exit(2);
    };
    let kodi = Kodi::new(var("KODI_URL", "http://127.0.0.1:8080"), var("KODI_USER", "kodi"), password);
    let state_dir = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("kodi-remote");
    let listen = Listen::new(var("GO2RTC_URL", "http://127.0.0.1:1984"), state_dir.join("sync-ms"));
    let bind = var("BIND", "0.0.0.0:8090");

    tokio::spawn(listen.clone().watchdog(kodi.clone()));
    tokio::spawn(listen.clone().keep_in_sync(kodi.clone()));

    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!("kodi-remote listening on http://{bind}");
    axum::serve(listener, routes::router(AppState { kodi, listen })).await
}
