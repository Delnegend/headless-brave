//! HTTP surface: the web UI on port 80, the CDP proxy on port 9222, and the
//! noVNC client the web UI is built from.

use std::path::{Path, PathBuf};

use axum::{
    Json, Router,
    extract::{Path as AxumPath, State, ws::WebSocketUpgrade},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;
use tracing::error;

use crate::{
    assets, bridge,
    bridge::Upstream,
    config::{Config, VncTarget},
};

/// The web UI, the noVNC client it loads, and the VNC bridge behind it.
pub fn router(config: &Config) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/api/config", get(api_config))
        .route("/websockify", get(websockify))
        .route("/novnc/{*path}", get(novnc_file))
        .route("/healthz", get(healthz))
        .route("/ws/cdp", get(ws_cdp))
        .with_state(config.clone())
}

/// The CDP proxy, on a port of its own so that `ws://host:9222/` keeps working
/// for tools that were configured against the proxy that preceded it.
pub fn cdp_router(config: &Config) -> Router {
    Router::new().fallback(ws_cdp).with_state(config.clone())
}

async fn index() -> Response {
    html(assets::INDEX_HTML)
}

async fn app_js() -> Response {
    // Our own page is compiled into the binary and changes with every image,
    // so a browser must not hold on to yesterday's copy.
    script(assets::APP_JS, "no-cache")
}

#[derive(Serialize)]
struct ApiConfig<'a> {
    vnc: &'a VncTarget,
}

async fn api_config(State(config): State<Config>) -> Response {
    Json(ApiConfig { vnc: &config.vnc }).into_response()
}

async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

async fn websockify(State(config): State<Config>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| async move {
        let address = (config.vnc.host.as_str(), config.vnc.port);
        match Upstream::tcp(address).await {
            Ok(upstream) => bridge::serve(socket, upstream).await,
            Err(connect_error) => {
                error!(%connect_error, "cannot reach the VNC server");
                bridge::close(
                    socket,
                    bridge::CLOSE_UPSTREAM_UNAVAILABLE,
                    &connect_error.to_string(),
                )
                .await;
            }
        }
    })
}

async fn ws_cdp(State(config): State<Config>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| async move {
        bridge::serve_cdp(socket, &config.cdp_version_url).await;
    })
}

async fn novnc_file(
    State(config): State<Config>,
    AxumPath(requested): AxumPath<String>,
) -> Response {
    match resolve_within(&config.novnc_dir, &requested) {
        Some(path) => match tokio::fs::read(&path).await {
            Ok(contents) => {
                ([(header::CONTENT_TYPE, content_type(&path))], contents).into_response()
            }
            Err(error) => {
                debug_missing(&path, &error);
                (StatusCode::NOT_FOUND, "not found\n").into_response()
            }
        },
        None => (StatusCode::BAD_REQUEST, "bad path\n").into_response(),
    }
}

fn debug_missing(path: &Path, error: &std::io::Error) {
    // A missing file is ordinary — the client asks for things it does not
    // use — so it is not worth failing over.
    if error.kind() != std::io::ErrorKind::NotFound {
        tracing::debug!(?path, ?error, "could not read an asset");
    }
}

/// Resolves `requested` under `root`, or `None` if it would escape.
///
/// The noVNC tree is a directory of scripts the browser asks for by name, so
/// it has to be walked — but nothing may climb out of it.
fn resolve_within(root: &Path, requested: &str) -> Option<PathBuf> {
    let mut path = root.to_path_buf();
    for segment in requested.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." || segment.contains('\0') {
            return None;
        }
        path.push(segment);
    }
    // Follow anything that resolves outside, which a symlink might.
    let root = root.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    path.starts_with(&root).then_some(path)
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("oga" | "mp3") => "audio/ogg",
        _ => "application/octet-stream",
    }
}

fn html(body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// A script, with how long a browser may keep it.
fn script(body: &'static str, cache_control: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, cache_control),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{content_type, resolve_within};

    #[test]
    fn refuses_a_path_that_does_not_exist() {
        let root = std::env::temp_dir();
        assert!(resolve_within(&root, "core/rfb.js").is_none());
    }

    #[test]
    fn refuses_to_climb_out_of_the_root() {
        let root = std::env::temp_dir();
        assert!(resolve_within(&root, "../etc/passwd").is_none());
        assert!(resolve_within(&root, "core/../../etc/passwd").is_none());
        assert!(resolve_within(&root, "..").is_none());
    }

    #[test]
    fn refuses_an_empty_or_null_segment() {
        let root = std::env::temp_dir();
        assert!(resolve_within(&root, "").is_none());
        assert!(resolve_within(&root, "core//rfb.js").is_none());
    }

    #[test]
    fn names_the_types_the_client_asks_for() {
        assert_eq!(
            content_type(Path::new("vnc.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("core/rfb.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("app/images/alt.svg")),
            "image/svg+xml"
        );
        assert_eq!(content_type(Path::new("defaults.json")), "application/json");
    }
}
