//! WebSocket server the bodies (ESP32, phone, later the desktop pet) connect to.
//! Everything on the broadcast channel goes to every client. Incoming messages:
//!   {"type":"hello","body":"phone"}  says what kind of body this is
//!   {"text":"..."}                   something the user said; gets a reply

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tauri::Emitter as _;
use tokio::sync::broadcast::{self, error::RecvError};
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Deserialize)]
struct BodyMessage {
    #[serde(rename = "type")]
    kind: Option<String>,
    body: Option<String>,
    text: Option<String>,
}

pub async fn start_server(
    app_handle: tauri::AppHandle,
    host: String,
    port: u16,
    broadcast_tx: broadcast::Sender<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    log::info!("WebSocket server listening on ws://{}", addr);
    app_handle.emit("ws:server_status", "connected").ok();

    loop {
        let (stream, client_addr) = listener.accept().await?;
        log::info!("New client connected from: {}", client_addr);
        let app_handle = app_handle.clone();
        let mut rx = broadcast_tx.subscribe();

        tokio::spawn(async move {
            let ws_stream = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    log::warn!("WebSocket handshake error from {}: {:?}", client_addr, e);
                    return;
                }
            };

            let (mut write, mut read) = ws_stream.split();
            app_handle
                .emit("ws:client_connected", client_addr.to_string())
                .ok();
            // Until the client says hello we don't know what it is
            let mut body = "body".to_string();

            loop {
                tokio::select! {
                    outgoing = rx.recv() => match outgoing {
                        Ok(msg) => {
                            if write.send(Message::Text(msg)).await.is_err() {
                                break;
                            }
                        }
                        // A slow client missed some messages (usually streamed tokens). Carry on.
                        Err(RecvError::Lagged(n)) => {
                            log::warn!("{} ({}) fell behind, skipped {} messages", client_addr, body, n);
                        }
                        Err(RecvError::Closed) => break,
                    },
                    incoming = read.next() => match incoming {
                        Some(Ok(Message::Text(text))) => {
                            let Ok(msg) = serde_json::from_str::<BodyMessage>(&text) else {
                                log::warn!("Ignoring non-JSON message from {}: {}", client_addr, text);
                                continue;
                            };
                            if msg.kind.as_deref() == Some("hello") {
                                if let Some(b) = msg.body.filter(|b| !b.trim().is_empty()) {
                                    log::info!("{} is a {}", client_addr, b);
                                    body = b;
                                }
                                continue;
                            }
                            if let Some(user_text) = msg.text {
                                let app = app_handle.clone();
                                let source = body.clone();
                                // Replies take a while; don't block this client's loop
                                tokio::spawn(async move {
                                    // Errors are already logged and sent to the bodies
                                    let _ = crate::companion::handle_user_message(&app, &source, &user_text).await;
                                });
                            }
                        }
                        Some(Ok(Message::Close(_))) | None => break,
                        Some(Ok(_)) => {} // ping/pong/binary
                        Some(Err(e)) => {
                            log::warn!("WebSocket read error from {}: {}", client_addr, e);
                            break;
                        }
                    },
                }
            }

            log::info!("Client {} disconnected", client_addr);
            app_handle
                .emit("ws:client_disconnected", client_addr.to_string())
                .ok();
        });
    }
}
