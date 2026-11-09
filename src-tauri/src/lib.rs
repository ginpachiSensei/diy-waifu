mod companion;
mod db;
mod monitor;
mod ollama;
mod phone;
mod settings;
mod websocket;

use db::{Db, StoredMessage};
use settings::Settings;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{Emitter, Manager};
use tokio::sync::broadcast;

pub struct AppState {
    pub broadcast_tx: broadcast::Sender<String>,
    pub ollama_child: Mutex<Option<tauri_plugin_shell::process::CommandChild>>,
    /// "starting" | "ready" | "failed"
    pub ollama_status: Mutex<&'static str>,
    pub paused: AtomicBool,
    /// Held while a reply is being generated, so replies happen one at a time
    pub chat_lock: tokio::sync::Mutex<()>,
}

fn set_ollama_status(app: &tauri::AppHandle, status: &'static str) {
    *app.state::<AppState>().ollama_status.lock().unwrap() = status;
    app.emit("ollama:status", status).ok();
}

// ---- Chat ----

/// Sends a message from the control panel. The reply also streams to the UI
/// through `chat:*` events and goes out to every connected body.
#[tauri::command]
async fn send_chat_message(app: tauri::AppHandle, text: String) -> Result<StoredMessage, String> {
    companion::handle_user_message(&app, "panel", &text).await
}

#[tauri::command]
fn get_history(db: tauri::State<'_, Db>, limit: Option<u32>) -> Result<Vec<StoredMessage>, String> {
    db.recent_messages(limit.unwrap_or(200))
}

#[tauri::command]
fn clear_history(db: tauri::State<'_, Db>) -> Result<(), String> {
    db.clear_messages()
}

// ---- Settings ----

#[tauri::command]
fn get_settings(db: tauri::State<'_, Db>) -> Settings {
    Settings::load(&db)
}

#[tauri::command]
fn save_settings(
    app: tauri::AppHandle,
    db: tauri::State<'_, Db>,
    settings: Settings,
) -> Result<Settings, String> {
    let settings = settings.cleaned()?;
    let old = Settings::load(&db);
    settings.save(&db)?;
    if old.model != settings.model {
        // Free the old model's RAM right away instead of waiting for keep_alive
        tauri::async_runtime::spawn(async move {
            ollama::unload_model(&old.model).await.ok();
        });
    }
    app.emit("settings:changed", &settings).ok();
    Ok(settings)
}

// ---- Models ----

#[tauri::command]
fn get_ollama_status(state: tauri::State<'_, AppState>) -> &'static str {
    *state.ollama_status.lock().unwrap()
}

#[tauri::command]
async fn list_models() -> Result<Vec<ollama::InstalledModel>, String> {
    ollama::list_models().await
}

/// Downloads a model, emitting `model:progress` while it goes.
#[tauri::command]
async fn pull_model(app: tauri::AppHandle, model: String) -> Result<(), String> {
    let model = model.trim().to_string();
    if model.is_empty() {
        return Err("Model name is empty".into());
    }
    log::info!("Pulling model {model}");
    ollama::pull_model(&model, |p| {
        app.emit(
            "model:progress",
            serde_json::json!({
                "model": model,
                "status": p.status,
                "completed": p.completed,
                "total": p.total,
            }),
        )
        .ok();
    })
    .await
}

#[tauri::command]
async fn delete_model(model: String) -> Result<(), String> {
    ollama::remove_model(&model).await
}

// ---- Companion ----

#[tauri::command]
async fn set_paused(
    paused: bool,
    state: tauri::State<'_, AppState>,
    db: tauri::State<'_, Db>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    state.paused.store(paused, Ordering::Relaxed);
    if paused {
        let model = Settings::load(&db).model;
        ollama::unload_model(&model).await.ok();
    }
    app_handle.emit("companion:paused", paused).ok();
    Ok(())
}

/// URL to open on a phone on the same Wi-Fi to use it as the companion's body.
#[tauri::command]
fn get_phone_url() -> Result<String, String> {
    let ip = phone::lan_ip().ok_or("Couldn't find this PC's network address")?;
    Ok(format!("http://{}:{}/", ip, phone::PHONE_PORT))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Big enough that streamed tokens don't push the final reply out for slow clients
    let (broadcast_tx, _) = broadcast::channel::<String>(512);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: None,
                    }),
                ])
                .level(log::LevelFilter::Info)
                .max_file_size(5_000_000)
                .rotation_strategy(tauri_plugin_log::RotationStrategy::KeepOne)
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .manage(AppState {
            broadcast_tx: broadcast_tx.clone(),
            ollama_child: Mutex::new(None),
            ollama_status: Mutex::new("starting"),
            paused: AtomicBool::new(false),
            chat_lock: tokio::sync::Mutex::new(()),
        })
        .invoke_handler(tauri::generate_handler![
            send_chat_message,
            get_history,
            clear_history,
            get_settings,
            save_settings,
            get_ollama_status,
            list_models,
            pull_model,
            delete_model,
            set_paused,
            get_phone_url
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("companion.db");
            log::info!("Database: {}", db_path.display());
            app.manage(Db::open(&db_path)?);

            let ws_handle = app_handle.clone();
            let ws_tx = broadcast_tx.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) =
                    websocket::start_server(ws_handle, "0.0.0.0".to_string(), 8080, ws_tx).await
                {
                    log::error!("WebSocket server error: {}", e);
                }
            });

            tauri::async_runtime::spawn(async move {
                if let Err(e) = phone::start_server("0.0.0.0".to_string(), phone::PHONE_PORT).await {
                    log::error!("Phone page server error: {}", e);
                }
            });

            let ollama_handle = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                match ollama::spawn_ollama(&ollama_handle) {
                    Ok(child) => {
                        let state = ollama_handle.state::<AppState>();
                        *state.ollama_child.lock().unwrap() = Some(child);

                        if ollama::wait_for_ollama_ready(20).await {
                            set_ollama_status(&ollama_handle, "ready");
                        } else {
                            log::error!("Ollama did not become ready in time");
                            set_ollama_status(&ollama_handle, "failed");
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to spawn Ollama sidecar: {}", e);
                        set_ollama_status(&ollama_handle, "failed");
                    }
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
                    log::info!("Ollama sidecar killed on exit");
                };
            }
        });
}
