//! `headless-brave-web` serves the container's web UI and the two WebSocket
//! bridges that reach the browser: VNC (the web UI) and CDP (automation).

mod assets;
mod bridge;
mod cdp;
mod config;
mod web;

use anyhow::{Context, Result};
use tokio::{net::TcpListener, sync::watch};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let config = Config::from_env()?;

    let web = TcpListener::bind(config.web_addr)
        .await
        .with_context(|| format!("cannot listen on {}", config.web_addr))?;
    let cdp = TcpListener::bind(config.cdp_addr)
        .await
        .with_context(|| format!("cannot listen on {}", config.cdp_addr))?;

    info!(
        web = %config.web_addr,
        cdp = %config.cdp_addr,
        vnc = ?config.vnc.port,
        browser = %config.cdp_version_url,
        "ready"
    );

    let (stopped, stopped_rx) = watch::channel(false);
    let web = axum::serve(web, web::router(&config))
        .with_graceful_shutdown(cancelled(stopped_rx.clone()));
    let cdp =
        axum::serve(cdp, web::cdp_router(&config)).with_graceful_shutdown(cancelled(stopped_rx));

    tokio::try_join!(web, cdp)?;
    let _ = stopped.send(true);
    info!("stopped");
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("headless_brave_web=info,warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

async fn cancelled(mut stopped: watch::Receiver<bool>) {
    if *stopped.borrow() {
        return;
    }
    if stopped.changed().await.is_err() {
        // The sender is gone, which only happens while shutting down.
        return;
    }
    warn!("shutdown requested");
}
