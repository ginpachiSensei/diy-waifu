use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tauri::Emitter as _;

#[derive(Debug, Deserialize)]
pub struct EspMessage {
    pub emotion: Option<String>,
    pub text: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct BroadcastData {
    pub emotion: Option<String>,
    pub text: Option<String>,
}

pub async fn start_server(
    app_handle: tauri::AppHandle,
    host: String,
    port: u16,
    broadcast_tx: broadcast::Sender<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("WebSocket server listening on ws://{}", addr);
    app_handle.emit("ws:server_status", "connected").ok();

    loop {
        let (stream, client_addr) = listener.accept().await?;
        println!("New client connected from: {}", client_addr);
        let app_handle = app_handle.clone();
        let mut rx = broadcast_tx.subscribe();

        tokio::spawn(async move {
            let ws_stream = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    eprintln!("WebSocket handshake error from {}: {:?}", client_addr, e);
                    return;
                }
            };

            let (mut write, mut read) = ws_stream.split();
            app_handle.emit("ws:client_connected", client_addr.to_string()).ok();

            loop {
                tokio::select! {
                    // Outgoing: anything broadcast from the chat bridge gets pushed to this client
                    Ok(msg) = rx.recv() => {
                        if write.send(Message::Text(msg)).await.is_err() {
                            break;
                        }
                    }
                    // Incoming: messages the ESP32 sends up to the PC (optional, kept for future use)
                    Some(result) = read.next() => {
                        match result {
                            Ok(Message::Text(text)) => {
                                println!("Received from {}: {}", client_addr, text);
                                if let Ok(parsed) = serde_json::from_str::<EspMessage>(&text) {
                                    let data = BroadcastData { emotion: parsed.emotion, text: parsed.text };
                                    app_handle.emit("ws:message", data).ok();
                                }
                            }
                            Ok(Message::Close(_)) | Err(_) => break,
                            _ => {}
                        }
                    }
                    else => break,
                }
            }

            println!("Client {} disconnected", client_addr);
            app_handle.emit("ws:client_disconnected", client_addr.to_string()).ok();
        });
    }
}
