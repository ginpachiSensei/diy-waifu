use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tauri::Emitter as _;
use tauri::Manager;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;

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
        let broadcast_tx = broadcast_tx.clone();

        tokio::spawn(async move {
            let ws_stream = match tokio_tungstenite::accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    eprintln!("WebSocket handshake error from {}: {:?}", client_addr, e);
                    return;
                }
            };

            let (mut write, mut read) = ws_stream.split();
            app_handle
                .emit("ws:client_connected", client_addr.to_string())
                .ok();

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

                                    // 1. Still emit to the frontend so your UI updates
                                    let data = BroadcastData {
                                        emotion: parsed.emotion.clone(),
                                        text: parsed.text.clone()
                                    };
                                    app_handle.emit("ws:message", data).ok();

                                    // 2. NEW: Call Ollama directly from the Rust backend!
                                    if let Some(user_text) = parsed.text {
                                        let state = app_handle.state::<crate::AppState>();

                                        // Check if companion is paused before answering
                                        if !state.paused.load(std::sync::atomic::Ordering::Relaxed) {
                                            let tx = broadcast_tx.clone();

                                            // Spawn a background task so we don't freeze the WebSocket loop
                                            tokio::spawn(async move {
                                                println!("Sending to Ollama: {}", user_text);

                                                match crate::ollama::chat(&user_text, crate::ollama::MODEL_NAME).await {
                                                    Ok((reply_text, emotion)) => {
                                                        println!("Got reply from Ollama: {}", reply_text);

                                                        // Format the payload exactly how the client expects it
                                                        let payload = serde_json::json!({
                                                            "emotion": emotion,
                                                            "text": reply_text
                                                        });

                                                        // This broadcast channel automatically routes back
                                                        // to the outgoing WebSocket select! macro loop
                                                        let _ = tx.send(payload.to_string());
                                                    }
                                                    Err(e) => eprintln!("❌ Ollama chat error: {}", e),
                                                }
                                            });
                                        }
                                    }
                                } else {
                                    eprintln!("Failed to parse JSON: {}", text);
                                }
                            }

                            // NEW: Handle other non-text WebSocket messages (Binary, Ping, Pong, Close)
                            Ok(other_msg) => {
                                println!("Received non-text message: {:?}", other_msg);
                            }

                            // NEW: Handle WebSocket connection errors
                            Err(e) => {
                                eprintln!("WebSocket read error: {}", e);
                                break; // Exit the loop because the connection is likely dead
                            }
                        }
                    }
                    else => break,
                }
            }

            println!("Client {} disconnected", client_addr);
            app_handle
                .emit("ws:client_disconnected", client_addr.to_string())
                .ok();
        });
    }
}
