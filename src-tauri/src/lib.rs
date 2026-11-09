mod activity;
mod companion;
mod db;
mod desktop;
mod monitor;
mod ollama;
mod pause;
mod phone;
mod settings;
mod websocket;

use db::{Db, StoredMessage};
use settings::Settings;
use pause::PauseState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as _;
use tokio::sync::broadcast;

/// Passed by the autostart entry: start in the tray without opening the panel
const MINIMIZED_ARG: &str = "--minimized";

pub struct AppState {
    pub broadcast_tx: broadcast::Sender<String>,
    pub ollama_child: Mutex<Option<tauri_plugin_shell::process::CommandChild>>,
    /// "starting" | "ready" | "failed"
    pub ollama_status: Mutex<&'static str>,
    /// Mirrors `pause.paused` for cheap checks; only pause.rs writes either
    pub paused: AtomicBool,
    pub pause: Mutex<PauseState>,
    pub shutting_down: AtomicBool,
    /// What the user is doing right now (see activity.rs)
    pub activity: Mutex<activity::Activity>,
    /// Held while a reply is being generated, so replies happen one at a time
    pub chat_lock: tokio::sync::Mutex<()>,
}

fn set_ollama_status(app: &tauri::AppHandle, status: &'static str) {
    let state = app.state::<AppState>();
    let mut current = state.ollama_status.lock().unwrap();
    if *current != status {
        *current = status;
        app.emit("ollama:status", status).ok();
    }
}

/// Starts the Ollama sidecar and restarts it if it stops answering
/// (checked every 15s; two misses in a row means it's gone).
async fn supervise_ollama(app: tauri::AppHandle) {
    let state = app.state::<AppState>();
    loop {
        set_ollama_status(&app, "starting");
        match ollama::spawn_ollama(&app) {
            Ok(child) => *state.ollama_child.lock().unwrap() = Some(child),
            Err(e) => log::error!("Failed to spawn Ollama sidecar: {}", e),
        }
        if ollama::wait_for_ollama_ready(40).await {
            set_ollama_status(&app, "ready");
        } else {
            log::error!("Ollama did not become ready in time");
            set_ollama_status(&app, "failed");
        }

        let mut misses = 0;
        while misses < 2 {
            tokio::time::sleep(Duration::from_secs(15)).await;
            if ollama::is_healthy().await {
                misses = 0;
                set_ollama_status(&app, "ready");
            } else {
                misses += 1;
            }
        }
        if state.shutting_down.load(Ordering::Relaxed) {
            return;
        }
        log::warn!("Ollama stopped responding, restarting it");
        set_ollama_status(&app, "failed");
        if let Some(child) = state.ollama_child.lock().unwrap().take() {
            child.kill().ok();
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
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

/// Manual pause. Auto-pause (heavy apps) is separate; see pause.rs.
#[tauri::command]
fn set_paused(app: tauri::AppHandle, paused: bool) -> PauseState {
    pause::set_manual(&app, paused);
    pause::get(&app)
}

#[tauri::command]
fn get_pause_state(app: tauri::AppHandle) -> PauseState {
    pause::get(&app)
}

// ---- Desktop: pet, autostart ----

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopState {
    pet_visible: bool,
    click_through: bool,
    autostart: bool,
}

#[tauri::command]
fn get_desktop_state(app: tauri::AppHandle) -> DesktopState {
    DesktopState {
        pet_visible: desktop::pet_visible(&app),
        click_through: desktop::click_through(&app),
        autostart: app.autolaunch().is_enabled().unwrap_or(false),
    }
}

#[tauri::command]
fn set_pet_visible(app: tauri::AppHandle, visible: bool) -> Result<(), String> {
    desktop::set_pet_visible(&app, visible)
}

#[tauri::command]
fn set_pet_click_through(app: tauri::AppHandle, on: bool) -> Result<(), String> {
    desktop::set_click_through(&app, on)
}

#[tauri::command]
fn set_autostart(app: tauri::AppHandle, enabled: bool) -> Result<(), String> {
    let launcher = app.autolaunch();
    if enabled {
        launcher.enable()
    } else {
        launcher.disable()
    }
    .map_err(|e| e.to_string())
}

#[tauri::command]
fn open_panel(app: tauri::AppHandle) {
    desktop::show_main(&app);
}

// ---- Activity ----

#[tauri::command]
fn get_activity(state: tauri::State<'_, AppState>) -> activity::Activity {
    state.activity.lock().unwrap().clone()
}

/// Milliseconds per category since `since` (unix millis). The panel passes
/// local midnight, which it knows and Rust (without a timezone crate) doesn't.
#[tauri::command]
fn get_activity_totals(db: tauri::State<'_, Db>, since: i64) -> Result<Vec<(String, i64)>, String> {
    db.category_totals_since(since)
}

/// GIF manifest plus where the GIFs are served, for the pet window.
#[tauri::command]
fn get_gif_manifest() -> serde_json::Value {
    let manifest: serde_json::Value =
        serde_json::from_str(phone::gif_manifest()).unwrap_or(serde_json::json!({}));
    serde_json::json!({
        "base": format!("http://127.0.0.1:{}/gifs/", phone::PHONE_PORT),
        "manifest": manifest,
    })
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
        // Must be first: a second launch just brings up the running app's panel
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            desktop::show_main(app);
        }))
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
            Some(vec![MINIMIZED_ARG]),
        ))
        .manage(AppState {
            broadcast_tx: broadcast_tx.clone(),
            ollama_child: Mutex::new(None),
            ollama_status: Mutex::new("starting"),
            paused: AtomicBool::new(false),
            pause: Mutex::new(PauseState::default()),
            shutting_down: AtomicBool::new(false),
            activity: Mutex::new(activity::Activity::default()),
            chat_lock: tokio::sync::Mutex::new(()),
        })
        .manage(desktop::PetState::default())
        .on_window_event(|window, event| match event {
            // Closing a window only hides it; Quit is in the tray
            tauri::WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                window.hide().ok();
                if window.label() == "pet" {
                    desktop::sync_tray(window.app_handle());
                }
            }
            tauri::WindowEvent::Moved(pos) if window.label() == "pet" => {
                desktop::pet_moved(window.app_handle(), *pos);
            }
            _ => {}
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
            get_pause_state,
            get_phone_url,
            get_desktop_state,
            set_pet_visible,
            set_pet_click_through,
            set_autostart,
            open_panel,
            get_gif_manifest,
            get_activity,
            get_activity_totals
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("companion.db");
            log::info!("Database: {}", db_path.display());
            app.manage(Db::open(&db_path)?);
            let settings = Settings::load(&app.state::<Db>());

            desktop::place_pet(&app_handle);
            if settings.pet_visible {
                if let Some(pet) = app.get_webview_window("pet") {
                    pet.show().ok();
                }
            }
            if !std::env::args().any(|a| a == MINIMIZED_ARG) {
                desktop::show_main(&app_handle);
            }
            if let Err(e) = desktop::build_tray(&app_handle) {
                // Without a tray there's no way to reopen a hidden panel
                log::error!("Couldn't create tray icon: {e}");
                desktop::show_main(&app_handle);
            }

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

            tauri::async_runtime::spawn(supervise_ollama(app_handle.clone()));

            tauri::async_runtime::spawn(activity::run(app_handle.clone()));

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
                state.shutting_down.store(true, Ordering::Relaxed);
                if let Some(child) = state.ollama_child.lock().unwrap().take() {
                    let _ = child.kill();
                    log::info!("Ollama sidecar killed on exit");
                };
            }
        });
}
