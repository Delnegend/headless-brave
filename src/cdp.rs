//! Discovery of the browser's `DevTools` WebSocket.
//!
//! Brave picks a random browser id on every start, so the WebSocket URL has to
//! be read from its HTTP endpoint each time a client connects.

use anyhow::{Context, Result, bail};
use http_body_util::{BodyExt, Empty};
use hyper::body::Bytes;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use serde::Deserialize;

/// WebSocket close code used when the browser is not reachable. Application
/// codes below 1000 are the only ones a server may pick; Chrome's own client
/// surfaces the accompanying reason text.
pub const CLOSE_UPSTREAM_UNAVAILABLE: u16 = 1000;

#[derive(Deserialize)]
struct Version {
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: String,
}

/// Fetches `version_url` and returns the browser's `DevTools` WebSocket URL.
pub async fn browser_websocket_url(version_url: &str) -> Result<String> {
    let uri: hyper::Uri = version_url
        .parse()
        .with_context(|| format!("{version_url} is not a valid URL"))?;

    let client: Client<HttpConnector, Empty<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    let response = client
        .get(uri)
        .await
        .with_context(|| format!("cannot query the DevTools endpoint at {version_url}"))?;
    if !response.status().is_success() {
        bail!(
            "the DevTools endpoint at {version_url} answered {}",
            response.status()
        );
    }
    let body = response
        .into_body()
        .collect()
        .await
        .context("cannot read the DevTools reply")?
        .to_bytes();

    let version: Version = serde_json::from_slice(&body)
        .with_context(|| format!("{version_url} did not return a /json/version document"))?;
    if version.web_socket_debugger_url.is_empty() {
        bail!("{version_url} does not advertise a WebSocket URL");
    }
    Ok(version.web_socket_debugger_url)
}
