//! WebSocket server the bodies (ESP32, phone, later the desktop pet) connect to.
//! Everything on the broadcast channel goes to every client. Incoming messages:
//!   {"type":"hello","body":"phone"}  says what kind of body this is
//!   {"text":"..."}                   something the user said; gets a reply
//!
//! Clients that aren't on this PC need the pairing code: ws://host:8080/?token=CODE
//! (see pairing.rs).

use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use tauri::{Emitter as _, Manager as _};
use tokio::sync::broadcast::{self, error::RecvError};
use crate::pairing::Verdict;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::Message;

#[derive(Debug, Deserialize)]
struct BodyMessage {
    #[serde(rename = "type")]
    kind: Option<String>,
    body: Option<String>,
    text: Option<String>,
    /// For {"type":"answer"}: which question, and the answer
    id: Option<u64>,
    yes: Option<bool>,
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
        let app_handle = app_handle.clone();
        let mut rx = broadcast_tx.subscribe();
        let mut token_rx = app_handle.state::<crate::pairing::Pairing>().0.subscribe();
        let local = crate::pairing::check(client_addr.ip(), None, "") == crate::pairing::Verdict::Ok;

        tokio::spawn(async move {
            let token = token_rx.borrow_and_update().clone();
            // The error type is fixed by tungstenite
            #[allow(clippy::result_large_err)]
            let check = |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                let given = crate::pairing::token_from_query(req.uri().query());
                match crate::pairing::check(client_addr.ip(), given.as_deref(), &token) {
                    Verdict::Ok => Ok(resp),
                    v => {
                        log::warn!("Refused body from {client_addr}: {v:?}");
                        let (code, text) = match v {
                            Verdict::NotLocal => (403, "Only devices on the local network can connect"),
                            _ => (401, "Wrong or missing pairing code (add ?token=CODE)"),
                        };
                        let mut r = ErrorResponse::new(Some(text.to_string()));
                        *r.status_mut() = StatusCode::from_u16(code).unwrap();
                        Err(r)
                    }
                }
            };
            let ws_stream = match tokio_tungstenite::accept_hdr_async(stream, check).await {
                Ok(ws) => ws,
                Err(e) => {
                    log::warn!("WebSocket handshake with {} failed: {}", client_addr, e);
                    return;
                }
            };
            log::info!("New client connected from: {}", client_addr);

            let (mut write, mut read) = ws_stream.split();
            app_handle
                .emit("ws:client_connected", client_addr.to_string())
                .ok();
            // Catch the new body up on what the user is doing
            let current = app_handle.state::<crate::AppState>().activity.lock().unwrap().clone();
            for msg in crate::activity::state_messages(&current) {
                if write.send(Message::Text(msg)).await.is_err() {
                    return;
                }
            }
            // Until the client says hello we don't know what it is
            let mut body = "body".to_string();

            loop {
                tokio::select! {
                    // A new pairing code was made: drop bodies that used the old one
                    _ = token_rx.changed(), if !local => {
                        log::info!("Disconnecting {} after the pairing code changed", client_addr);
                        write.send(Message::Close(None)).await.ok();
                        break;
                    }
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
                            if msg.kind.as_deref() == Some("answer") {
                                if let (Some(id), Some(yes)) = (msg.id, msg.yes) {
                                    crate::ask::answer(&app_handle, id, yes);
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
