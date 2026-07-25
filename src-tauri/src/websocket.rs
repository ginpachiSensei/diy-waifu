use futures::sink::SinkExt;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
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

pub async fn start_server(app_handle: tauri::AppHandle, host: String, port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("{}:{}", host, port);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    println!("WebSocket server listening on ws://{}", addr);

    // Emit event that server is ready
    app_handle.emit("ws:server_status", "connected").ok();

    loop {
        let (stream, client_addr) = listener.accept().await?;
        println!("New client connected from: {}", client_addr);

        let app_handle = app_handle.clone();

        // Emit event that a new client connected
        app_handle.emit("ws:client_connected", client_addr.to_string()).ok();

        tokio::spawn(async move {
            use tokio_tungstenite::accept_async;

            let ws_stream = match accept_async(stream).await {
                Ok(ws) => ws,
                Err(e) => {
                    eprintln!("WebSocket accept error: {}", e);
                    return;
                }
            };

            let (mut write, mut read) = ws_stream.split();

            // Handle reading and sending
            while let Some(result) = read.next().await {
                let message = match result {
                    Ok(msg) => msg,
                    Err(e) => {
                        eprintln!("WebSocket error: {}", e);
                        break;
                    }
                };

                // Only process text messages
                if let Message::Text(text) = &message {
                    println!("Received: {}", text);

                    // Try to parse as EspMessage
                    let parsed: Result<EspMessage, _> = serde_json::from_str(text);

                    let broadcast_msg = match &parsed {
                        Ok(msg) => {
                            // Serialize first for the acknowledgment
                            let data = BroadcastData {
                                emotion: msg.emotion.clone(),
                                text: msg.text.clone(),
                            };
                            let json_str = serde_json::to_string(&data).unwrap_or_default();
                            // Forward emotion/text to frontend
                            app_handle.emit("ws:message", data).ok();
                            json_str
                        }
                        Err(_) => {
                            // Not a structured message, just forward raw
                            text.clone()
                        }
                    };

                    // Echo back acknowledgment
                    let ack = format!(r#"{{"status":"received","data":"{}"}}"#, broadcast_msg.replace('"', "\\\""));
                    if write.send(Message::Text(ack)).await.is_err() {
                        break;
                    }
                }
            }

            println!("Client {} disconnected", client_addr);
            app_handle.emit("ws:client_disconnected", client_addr.to_string()).ok();
        });
    }
}
