//! Runtime configuration, read once from the environment.

use std::{env, net::SocketAddr, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

/// The VNC server the web UI attaches to. The page has nothing to configure,
/// so these settings decide what every visitor sees.
#[derive(Clone, Debug, Serialize)]
pub struct VncTarget {
    pub host: String,
    pub port: u16,
    pub password: String,
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Address for the web UI and its WebSocket bridge.
    pub web_addr: SocketAddr,
    /// Address for the CDP WebSocket proxy, kept separate so existing
    /// `ws://host:9222/` client configurations keep working.
    pub cdp_addr: SocketAddr,
    /// x11vnc, the desktop the web UI shows.
    pub vnc: VncTarget,
    /// Brave's `DevTools` HTTP endpoint, used to discover the browser WebSocket.
    pub cdp_version_url: String,
    /// Where the noVNC client is served from, installed by the distribution.
    pub novnc_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let bind = string("BIND_ADDR", "0.0.0.0");
        let web_port = number("WEB_PORT", 80)?;
        let cdp_port = number("CDP_PORT", 9222)?;

        let vnc = VncTarget {
            host: string("VNC_HOST", "127.0.0.1"),
            port: number("VNC_PORT", 5900)?,
            password: string("VNC_PASSWORD", "headless"),
        };
        if vnc.password.is_empty() {
            bail!("VNC_PASSWORD must not be empty");
        }

        Ok(Self {
            web_addr: resolve(&bind, web_port)?,
            cdp_addr: resolve(&bind, cdp_port)?,
            vnc,
            cdp_version_url: string("BROWSER_CDP_URL", "http://127.0.0.1:9224/json/version"),
            novnc_dir: PathBuf::from(string("NOVNC_DIR", "/usr/share/novnc")),
        })
    }
}

fn resolve(bind: &str, port: u16) -> Result<SocketAddr> {
    format!("{bind}:{port}")
        .parse()
        .with_context(|| format!("cannot build a listen address from {bind}:{port}"))
}

fn string(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn number(key: &str, default: u16) -> Result<u16> {
    env::var(key).map_or_else(
        |_| Ok(default),
        |raw| {
            raw.parse()
                .with_context(|| format!("{key} must be a port number, got {raw:?}"))
        },
    )
}
