//! WebSocket bridges.
//!
//! Two things sit behind the browser and neither is a WebSocket: the VNC
//! server that carries the desktop, and the browser's own `DevTools` endpoint,
//! whose address is not known until the browser is asked. One relay, two
//! flavours of upstream.

use std::io;

use axum::extract::ws::{CloseFrame, Message as ClientMessage, Utf8Bytes, WebSocket};
use bytes::Bytes;
use futures_util::{
    SinkExt, StreamExt,
    stream::{SplitSink, SplitStream},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpStream, ToSocketAddrs,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
};
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::debug;

use crate::cdp;

/// A message in flight between the browser and an upstream, tagged with the
/// frame type it travels in so that text stays text. The payload is bytes,
/// not a string: a VNC session is binary from the first challenge on, and
/// anything that is not valid UTF-8 would be mangled in transit.
struct Frame {
    payload: Bytes,
    text: bool,
}

/// The service a browser WebSocket is relayed to, split into the two halves
/// the relay directions each own.
pub struct Upstream {
    reader: Reader,
    writer: Writer,
}

/// Where an upstream's bytes come from.
enum Reader {
    Tcp(Tcp),
    Ws(SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

struct Tcp {
    stream: OwnedReadHalf,
}

enum Writer {
    Tcp(OwnedWriteHalf),
    Ws(SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, UpstreamMessage>),
}

impl Upstream {
    pub async fn tcp<A>(address: A) -> io::Result<Self>
    where
        A: ToSocketAddrs,
    {
        let (reader, writer) = TcpStream::connect(address).await?.into_split();
        Ok(Self {
            reader: Reader::Tcp(Tcp { stream: reader }),
            writer: Writer::Tcp(writer),
        })
    }

    async fn websocket(url: &str) -> Result<Self, String> {
        let (socket, response) = connect_async(url)
            .await
            .map_err(|error| format!("cannot connect to {url}: {error}"))?;
        debug!(status = ?response.status(), "upstream websocket established");
        let (writer, reader) = socket.split();
        Ok(Self {
            reader: Reader::Ws(reader),
            writer: Writer::Ws(writer),
        })
    }
}

impl Reader {
    /// The next message for the browser. A TCP upstream carries a binary
    /// protocol, so its chunks go out as binary frames; a WebSocket upstream
    /// keeps the frame type it arrived with.
    async fn recv(&mut self) -> io::Result<Option<Frame>> {
        match self {
            Self::Tcp(tcp) => {
                let mut buffer = vec![0u8; 16 * 1024];
                let read = tcp.stream.read(&mut buffer).await?;
                if read == 0 {
                    return Ok(None);
                }
                buffer.truncate(read);
                Ok(Some(Frame {
                    payload: Bytes::from(buffer),
                    text: false,
                }))
            }
            Self::Ws(socket) => loop {
                match socket.next().await {
                    None => return Ok(None),
                    Some(Err(error)) => return Err(io::Error::other(error)),
                    Some(Ok(message)) => match message {
                        UpstreamMessage::Text(text) => {
                            return Ok(Some(Frame {
                                payload: Bytes::copy_from_slice(text.as_str().as_bytes()),
                                text: true,
                            }));
                        }
                        UpstreamMessage::Binary(payload) => {
                            return Ok(Some(Frame {
                                payload,
                                text: false,
                            }));
                        }
                        // tungstenite answers pings for us, but only once the
                        // socket is polled for writing; anything else ends
                        // the stream.
                        UpstreamMessage::Ping(_) | UpstreamMessage::Pong(_) => {}
                        _ => return Ok(None),
                    },
                }
            },
        }
    }
}

impl Writer {
    async fn write(&mut self, payload: &[u8], text: bool) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.write_all(payload).await,
            Self::Ws(socket) => {
                let message = if text {
                    let text = String::from_utf8_lossy(payload);
                    UpstreamMessage::text(text.as_ref())
                } else {
                    UpstreamMessage::Binary(Bytes::copy_from_slice(payload))
                };
                socket.send(message).await.map_err(io::Error::other)
            }
        }
    }

    async fn shutdown(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.shutdown().await,
            Self::Ws(socket) => socket.close().await.map_err(io::Error::other),
        }
    }
}

/// Relays the browser and the upstream in both directions until either side
/// closes. Nothing is interpreted: a VNC session is a byte stream, and a
/// `DevTools` session is already framed.
pub async fn serve(client: WebSocket, Upstream { reader, writer }: Upstream) {
    let (client_tx, client_rx) = client.split();
    let _ = tokio::join!(
        tokio::spawn(relay_to_upstream(client_rx, writer)),
        tokio::spawn(relay_to_client(reader, client_tx)),
    );
}

async fn relay_to_upstream(mut client: SplitStream<WebSocket>, mut writer: Writer) {
    while let Some(message) = client.next().await {
        let (payload, text) = match message {
            Ok(ClientMessage::Text(text)) => {
                (Bytes::copy_from_slice(text.as_str().as_bytes()), true)
            }
            Ok(ClientMessage::Binary(payload)) => (payload, false),
            // The axum socket already answers pings for us.
            Ok(ClientMessage::Close(_) | ClientMessage::Ping(_) | ClientMessage::Pong(_)) => break,
            Err(error) => {
                debug!(%error, "client websocket failed");
                break;
            }
        };
        if let Err(error) = writer.write(&payload, text).await {
            debug!(%error, "upstream write failed");
            break;
        }
    }
    if let Err(error) = writer.shutdown().await {
        debug!(%error, "upstream shutdown failed");
    }
}

async fn relay_to_client(mut reader: Reader, mut client: SplitSink<WebSocket, ClientMessage>) {
    while let Ok(Some(frame)) = reader.recv().await {
        let message = if frame.text {
            let text = String::from_utf8_lossy(&frame.payload);
            ClientMessage::Text(Utf8Bytes::from(text.as_ref()))
        } else {
            ClientMessage::Binary(frame.payload)
        };
        if let Err(error) = client.send(message).await {
            debug!(%error, "client write failed");
            break;
        }
    }
}

/// Resolves the browser's `DevTools` WebSocket and relays the client to it.
pub async fn serve_cdp(client: WebSocket, version_url: &str) {
    let upstream = match cdp::browser_websocket_url(version_url).await {
        Ok(url) => match Upstream::websocket(&url).await {
            Ok(upstream) => upstream,
            Err(error) => {
                debug!(%error, "no browser session on the DevTools endpoint");
                close(client, cdp::CLOSE_UPSTREAM_UNAVAILABLE, &error).await;
                return;
            }
        },
        Err(error) => {
            debug!(%error, "cannot reach the DevTools endpoint");
            close(client, cdp::CLOSE_UPSTREAM_UNAVAILABLE, &error.to_string()).await;
            return;
        }
    };
    serve(client, upstream).await;
}

/// WebSocket close code used when the service on the other end is not there.
/// Application codes below 1000 are the only ones a server may pick.
pub const CLOSE_UPSTREAM_UNAVAILABLE: u16 = 1000;

/// Closes a client's tunnel with a reason the peer can report.
pub async fn close(mut client: WebSocket, code: u16, reason: &str) {
    // A close reason is capped at 123 bytes by the protocol. Cutting one on a
    // byte count would split a character, so stop on a character boundary.
    let reason = truncate(reason, CLOSE_REASON_LIMIT);
    if let Err(error) = client
        .send(ClientMessage::Close(Some(CloseFrame {
            code,
            reason: Utf8Bytes::from(reason),
        })))
        .await
    {
        debug!(%error, "cannot report the failure to the client");
    }
}

/// How many bytes of a close reason the protocol allows, leaving room for the
/// ellipsis added when one has to be cut.
const CLOSE_REASON_LIMIT: usize = 120;

/// Cuts `value` to at most `limit` bytes without splitting a character.
fn truncate(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let end = value
        .char_indices()
        .map(|(index, _)| index)
        .take_while(|index| *index <= limit)
        .last()
        .unwrap_or(0);
    format!("{}…", value.get(..end).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::{CLOSE_REASON_LIMIT, truncate};

    #[test]
    fn leaves_a_short_reason_alone() {
        assert_eq!(
            truncate("cannot reach vnc", CLOSE_REASON_LIMIT),
            "cannot reach vnc"
        );
    }

    #[test]
    fn cuts_a_long_reason_without_splitting_a_character() {
        // Every one of these is longer than the limit once the multi-byte
        // characters are counted, so a byte-count cut would panic here.
        let reason = "ü".repeat(200);
        let cut = truncate(&reason, CLOSE_REASON_LIMIT);
        assert!(cut.len() <= CLOSE_REASON_LIMIT + '…'.len_utf8());
        assert!(cut.ends_with('…'));
        assert!(reason.starts_with(cut.strip_suffix('…').unwrap_or_default()));
    }
}
