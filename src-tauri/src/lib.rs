mod monitor;
mod ollama;
mod websocket;

use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio::sync::broadcast;

pub struct AppState {
    pub broadcast_tx: broadcast::Sender<String>,
    pub ollama_child: Mutex<Option<tauri_plugin_shell::process::CommandChild>>,
}

#[tauri::command]
async fn send_chat_message(
    text: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    let (reply_text, emotion) = ollama::chat(&text, "qwen2.5:1.5b").await?;

    let payload = serde_json::json!({ "emotion": emotion, "text": reply_text });
    state.broadcast_tx.send(payload.to_string()).ok();

    Ok(reply_text)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let (broadcast_tx, _) = broadcast::channel::<String>(16);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(AppState {
            broadcast_tx: broadcast_tx.clone(),
            ollama_child: Mutex::new(None),
        })
        .invoke_handler(tauri::generate_handler![send_chat_message])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            // 1. Start WebSocket server
            let ws_handle = app_handle.clone();
            let ws_tx = broadcast_tx.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) =
                    websocket::start_server(ws_handle, "0.0.0.0".to_string(), 8080, ws_tx).await
                {
                    eprintln!("WebSocket server error: {}", e);
                }
            });

            // 2. Spawn Ollama sidecar, wait until ready
            let ollama_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                match ollama::spawn_ollama(&ollama_handle) {
                    Ok(child) => {
                        let state = ollama_handle.state::<AppState>();
                        *state.ollama_child.lock().unwrap() = Some(child);

                        if ollama::wait_for_ollama_ready(20).await {
                            println!("Ollama is up and ready");
                        } else {
                            eprintln!("Ollama did not become ready in time");
                        }
                    }
                    Err(e) => eprintln!("Failed to spawn Ollama sidecar: {}", e),
                }
            });

            // 4. Start GPU/process monitor loop
            let monitor_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                monitor::run_monitor_loop(monitor_handle).await;
            });

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state = app_handle.state::<AppState>();
                if let Some(child) = state.ollama_child.lock().unwrap().take() {
                    let _ = child.kill();
                    println!("Ollama sidecar killed on exit");
                }
            }
        });
}
