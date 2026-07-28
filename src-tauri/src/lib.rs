mod monitor;
mod ollama;
mod websocket;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio::sync::broadcast;

pub struct AppState {
    pub broadcast_tx: broadcast::Sender<String>,
    pub ollama_child: Mutex<Option<tauri_plugin_shell::process::CommandChild>>,
    pub paused: AtomicBool,
}

#[tauri::command]
async fn send_chat_message(
    text: String,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    if state.paused.load(Ordering::Relaxed) {
        return Err("Companion is paused".into());
    }

    let (reply_text, emotion) = ollama::chat(&text, ollama::MODEL_NAME).await?;
    let payload = serde_json::json!({ "emotion": emotion, "text": reply_text });
    state.broadcast_tx.send(payload.to_string()).ok();
    Ok(reply_text)
}

#[tauri::command]
async fn install_model(app_handle: tauri::AppHandle) -> Result<(), String> {
    app_handle.emit("model:status", "installing").ok();
    ollama::pull_model(ollama::MODEL_NAME).await?;
    app_handle.emit("model:status", "installed").ok();
    Ok(())
}

#[tauri::command]
async fn uninstall_model(app_handle: tauri::AppHandle) -> Result<(), String> {
    app_handle.emit("model:status", "uninstalling").ok();
    ollama::remove_model(ollama::MODEL_NAME).await?;
    app_handle.emit("model:status", "not_installed").ok();
    Ok(())
}

#[tauri::command]
async fn check_model_status(app_handle: tauri::AppHandle) -> Result<bool, String> {
    let installed = ollama::is_model_installed(ollama::MODEL_NAME).await;
    app_handle
        .emit(
            "model:status",
            if installed {
                "installed"
            } else {
                "not_installed"
            },
        )
        .ok();
    Ok(installed)
}

#[tauri::command]
async fn set_paused(
    paused: bool,
    state: tauri::State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.paused.store(paused, Ordering::Relaxed);
    if paused {
        ollama::unload_model(ollama::MODEL_NAME).await.ok();
    }
    app_handle.emit("companion:paused", paused).ok();
    Ok(())
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
            paused: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            send_chat_message,
            install_model,
            uninstall_model,
            check_model_status,
            set_paused
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            let ws_handle = app_handle.clone();
            let ws_tx = broadcast_tx.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) =
                    websocket::start_server(ws_handle, "0.0.0.0".to_string(), 8080, ws_tx).await
                {
                    eprintln!("WebSocket server error: {}", e);
                }
            });

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
                };
            }
        });
}
