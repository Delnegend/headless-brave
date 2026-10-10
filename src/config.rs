//! Runtime configuration, read once from the environment.

use std::{env, fmt, net::SocketAddr, path::PathBuf, str::FromStr};

use anyhow::{Context, Result, bail};
use serde::Serialize;

/// The VNC server the web UI attaches to. The page has nothing to configure,
/// so these settings decide what every visitor sees.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct VncTarget {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) password: String,
}

/// The screen the browser is drawn on, and the browser itself.
#[derive(Clone, Debug)]
pub(crate) struct Session {
    /// X display number, without the colon.
    pub(crate) display: u16,
    pub(crate) resolution: Resolution,
    /// Where the browser keeps its profile.
    pub(crate) profile: PathBuf,
    pub(crate) brave: Brave,
}

/// The browser's install, which lives on a volume rather than in the image.
#[derive(Clone, Debug)]
pub(crate) struct Brave {
    /// Where the payload is installed.
    pub(crate) root: PathBuf,
}

impl Brave {
    /// The browser itself, which is a file rather than the launcher the
    /// package installs: the launcher is the wrapper script next to it, which
    /// runs this and reports success whatever it did.
    pub(crate) fn binary(&self) -> PathBuf {
        self.root.join("brave-origin").join("brave")
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Resolution {
    pub(crate) width: u32,
    pub(crate) height: u32,
}

impl fmt::Display for Resolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}x{}", self.width, self.height)
    }
}

impl FromStr for Resolution {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self> {
        let parse = |part: &str| -> Result<u32> {
            part.parse()
                .with_context(|| format!("{part} is not a number of pixels"))
        };
        match value.split_once('x') {
            Some((width, height)) => Ok(Self {
                width: parse(width)?,
                height: parse(height)?,
            }),
            None => bail!("expected WIDTHxHEIGHT, got {value:?}"),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Config {
    /// Address for the web UI and its WebSocket bridge.
    pub(crate) web_addr: SocketAddr,
    /// Address for the CDP WebSocket proxy, kept separate so existing
    /// `ws://host:9222/` client configurations keep working. // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket -- doc comment naming a legacy client URL, not a connection this code makes
    pub(crate) cdp_addr: SocketAddr,
    pub(crate) vnc: VncTarget,
    /// Brave's `DevTools` HTTP endpoint, used to discover the browser WebSocket.
    pub(crate) cdp_version_url: String,
    /// Where the noVNC client is served from, installed by the distribution.
    pub(crate) novnc_dir: PathBuf,
    pub(crate) session: Session,
}

impl Config {
    pub(crate) fn from_env() -> Result<Self> {
        let bind = string("BIND_ADDR", "0.0.0.0");
        // 8000, not 80: the process is unprivileged, so it cannot bind a
        // privileged port, and the image says the same thing.
        let web_port = number("WEB_PORT", 8000)?;
        let cdp_port = number("CDP_PORT", 9222)?;

        let vnc = VncTarget {
            host: string("VNC_HOST", "127.0.0.1"),
            port: number("VNC_PORT", 5900)?,
            password: string("VNC_PASSWORD", "headless"),
        };
        if vnc.password.is_empty() {
            bail!("VNC_PASSWORD must not be empty");
        }

        let display = string("VNC_DISPLAY", ":99");
        let display = display
            .strip_prefix(':')
            .and_then(|number| number.parse().ok())
            .with_context(|| format!("VNC_DISPLAY must look like :99, got {display:?}"))?;

        let profile = path("BRAVE_PROFILE", "/data/profile")?;
        let root = path("BRAVE_ROOT", "/opt/brave.com")?;

        Ok(Self {
            web_addr: resolve(&bind, web_port)?,
            cdp_addr: resolve(&bind, cdp_port)?,
            vnc,
            cdp_version_url: string("BROWSER_CDP_URL", "http://127.0.0.1:9224/json/version"),
            novnc_dir: path("NOVNC_DIR", "/usr/share/novnc")?,
            session: Session {
                display,
                resolution: string("RESOLUTION", "1920x1080").parse()?,
                profile,
                brave: Brave { root },
            },
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

fn path(key: &str, default: &str) -> Result<PathBuf> {
    let value = string(key, default);
    if !value.starts_with('/') {
        bail!("{key} must be an absolute path, got {value:?}");
    }
    Ok(PathBuf::from(value))
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

#[cfg(test)]
mod tests {
    use super::Resolution;

    #[test]
    fn reads_a_resolution() {
        let parsed: Resolution = "1920x1080".parse().expect("parses");
        assert_eq!((parsed.width, parsed.height), (1920, 1080));
    }

    #[test]
    fn round_trips_through_its_display_form() {
        let resolution = Resolution {
            width: 800,
            height: 600,
        };
        assert_eq!(resolution.to_string(), "800x600");
    }

    #[test]
    fn refuses_a_resolution_it_cannot_use() {
        assert!("1920".parse::<Resolution>().is_err());
        assert!("1920x".parse::<Resolution>().is_err());
        assert!("x1080".parse::<Resolution>().is_err());
        assert!("19twenty.x1080".parse::<Resolution>().is_err());
    }
}
