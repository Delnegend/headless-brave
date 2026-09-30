//! `headless-brave-web` runs the container: the browser's screen, the browser,
//! the VNC server, and the web service that lets a browser watch the screen
//! and a tool drive it.

mod assets;
mod bridge;
mod cdp;
mod config;
mod supervise;
mod web;

use anyhow::Result;
use tracing::info;
use tracing_subscriber::EnvFilter;

use config::Config;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();
    let config = Config::from_env()?;
    info!("headless-brave starting");
    supervise::run(config).await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("headless_brave_web=info,warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
