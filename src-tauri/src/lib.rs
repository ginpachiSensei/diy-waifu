mod activity;
mod ask;
mod companion;
mod db;
mod desktop;
mod esp32;
mod ollama;
mod memory;
mod pairing;
mod pause;
mod proactive;
mod reminders;
mod phone;
mod settings;
mod tools;
mod voice;
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
    if old.ptt_shortcut != settings.ptt_shortcut || old.voice_input != settings.voice_input {
        if let Some(problem) = voice::register_shortcut(&app) {
            return Err(problem);
        }
    }
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

/// The pet page reports where its visible parts are (see desktop::pass_through_loop)
#[tauri::command]
fn set_pet_hit_rects(app: tauri::AppHandle, rects: Vec<desktop::HitRect>) {
    desktop::set_hit_rects(&app, rects);
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

/// Runs one search with the given Brave key (empty: DuckDuckGo Instant
/// Answers) so the System tab can check a key before saving it.
#[tauri::command]
async fn test_search(brave_key: String) -> Result<String, String> {
    let out = tools::search::search("weather", &brave_key).await?;
    Ok(format!("{} works ({} results)", out.provider, out.results.len()))
}

// ---- Memory ----

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct MemoryStatus {
    embed_model: &'static str,
    embed_ready: bool,
    facts: i64,
    summaries: i64,
    messages: i64,
    embedded_messages: i64,
}

#[tauri::command]
async fn memory_status(db: tauri::State<'_, Db>) -> Result<MemoryStatus, String> {
    Ok(MemoryStatus {
        embed_model: memory::EMBED_MODEL,
        embed_ready: memory::embed_ready().await,
        facts: db.count("facts")?,
        summaries: db.count("summaries")?,
        messages: db.count("messages")?,
        embedded_messages: db.count("message_embeddings")?,
    })
}

#[tauri::command]
fn list_facts(db: tauri::State<'_, Db>) -> Result<Vec<db::Fact>, String> {
    db.list_facts()
}

#[tauri::command]
async fn add_fact(app: tauri::AppHandle, db: tauri::State<'_, Db>, text: String) -> Result<db::Fact, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("The memory is empty".into());
    }
    let emb = memory::embed_document(text).await;
    let f = db.add_fact(text, "user", emb.as_deref())?;
    app.emit("memory:changed", ()).ok();
    Ok(f)
}

#[tauri::command]
async fn update_fact(app: tauri::AppHandle, db: tauri::State<'_, Db>, id: i64, text: String) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("The memory is empty".into());
    }
    let emb = memory::embed_document(text).await;
    db.update_fact(id, text, emb.as_deref())?;
    app.emit("memory:changed", ()).ok();
    Ok(())
}

#[tauri::command]
fn delete_fact(app: tauri::AppHandle, db: tauri::State<'_, Db>, id: i64) -> Result<(), String> {
    db.delete_fact(id)?;
    app.emit("memory:changed", ()).ok();
    Ok(())
}

#[tauri::command]
fn list_summaries(db: tauri::State<'_, Db>) -> Result<Vec<db::Summary>, String> {
    db.list_summaries(100)
}

#[tauri::command]
fn delete_summary(app: tauri::AppHandle, db: tauri::State<'_, Db>, id: i64) -> Result<(), String> {
    db.delete_summary(id)?;
    app.emit("memory:changed", ()).ok();
    Ok(())
}

#[tauri::command]
fn list_reminders(db: tauri::State<'_, Db>) -> Result<Vec<db::Reminder>, String> {
    db.pending_reminders()
}

#[tauri::command]
fn delete_reminder(app: tauri::AppHandle, db: tauri::State<'_, Db>, id: i64) -> Result<(), String> {
    db.delete_reminder(id)?;
    app.emit("memory:changed", ()).ok();
    Ok(())
}

/// Facts, summaries and message embeddings. Chat history stays.
#[tauri::command]
fn clear_memory(app: tauri::AppHandle, db: tauri::State<'_, Db>) -> Result<(), String> {
    db.clear_memory()?;
    app.emit("memory:changed", ()).ok();
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct MemoryExport {
    version: u32,
    facts: Vec<db::Fact>,
    summaries: Vec<db::Summary>,
}

#[tauri::command]
fn export_memory(db: tauri::State<'_, Db>) -> Result<String, String> {
    let export = MemoryExport {
        version: 1,
        facts: db.list_facts()?,
        summaries: db.list_summaries(10_000)?,
    };
    serde_json::to_string_pretty(&export).map_err(|e| e.to_string())
}

/// Adds facts from an export (skipping ones already known). Returns how many were added.
#[tauri::command]
async fn import_memory(app: tauri::AppHandle, db: tauri::State<'_, Db>, json: String) -> Result<usize, String> {
    let data: MemoryExport =
        serde_json::from_str(&json).map_err(|e| format!("That isn't a memory export: {e}"))?;
    let known: Vec<String> = db.list_facts()?.into_iter().map(|f| f.text.to_lowercase()).collect();
    let mut added = 0;
    for f in data.facts {
        if f.text.trim().is_empty() || known.contains(&f.text.to_lowercase()) {
            continue;
        }
        let emb = memory::embed_document(&f.text).await;
        db.add_fact(&f.text, &f.origin, emb.as_deref())?;
        added += 1;
    }
    app.emit("memory:changed", ()).ok();
    Ok(added)
}

// ---- Voice ----

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct VoiceStatus {
    models: Vec<voice::ModelInfo>,
    microphone: Option<String>,
    recording: bool,
}

#[tauri::command]
fn voice_status(app: tauri::AppHandle) -> VoiceStatus {
    VoiceStatus {
        models: voice::list_models(&app),
        microphone: voice::input_device_name(),
        recording: voice::is_recording(&app),
    }
}

#[tauri::command]
async fn download_voice_model(app: tauri::AppHandle, id: String) -> Result<(), String> {
    voice::download_model(&app, &id).await
}

#[tauri::command]
async fn delete_voice_model(app: tauri::AppHandle, id: String) -> Result<(), String> {
    voice::delete_model(&app, &id).await
}

/// Answers an "ask me first" question (see ask.rs)
#[tauri::command]
fn answer_ask(app: tauri::AppHandle, id: u64, yes: bool) -> bool {
    ask::answer(&app, id, yes)
}

/// Pet mic button pressed
#[tauri::command]
fn voice_start(app: tauri::AppHandle) -> Result<(), String> {
    voice::start(&app)
}

/// Pet mic button released
#[tauri::command]
async fn voice_stop(app: tauri::AppHandle) {
    voice::stop_and_send(&app).await;
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

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Pairing {
    code: String,
    /// This PC on the local network, if it has an address there
    lan_ip: Option<String>,
    /// Page to open on a phone, with the code in it
    phone_url: Option<String>,
    /// QR code of `phone_url`
    qr_svg: Option<String>,
}

/// The pairing code and the phone link, for the Bodies tab.
#[tauri::command]
fn get_pairing(pairing: tauri::State<'_, pairing::Pairing>) -> Pairing {
    let code = pairing.current();
    let lan_ip = phone::lan_ip().map(|ip| ip.to_string());
    let phone_url = lan_ip
        .as_ref()
        .map(|ip| format!("http://{}:{}/?token={}", ip, phone::PHONE_PORT, code));
    let qr_svg = phone_url.as_deref().and_then(|u| pairing::qr_svg(u).ok());
    Pairing { code, lan_ip, phone_url, qr_svg }
}

/// Makes a new pairing code. Bodies on other devices are disconnected and need the new one.
#[tauri::command]
fn new_pairing_code(
    pairing: tauri::State<'_, pairing::Pairing>,
    db: tauri::State<'_, Db>,
) -> Result<Pairing, String> {
    pairing.regenerate(&db)?;
    Ok(get_pairing(pairing))
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
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    use tauri_plugin_global_shortcut::ShortcutState;
                    voice::on_shortcut(app, event.state() == ShortcutState::Pressed);
                })
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
        .manage(voice::VoiceState::default())
        .manage(ask::Asks::default())
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
            get_pairing,
            new_pairing_code,
            get_desktop_state,
            set_pet_visible,
            set_pet_click_through,
            set_pet_hit_rects,
            set_autostart,
            open_panel,
            get_gif_manifest,
            get_activity,
            get_activity_totals,
            test_search,
            memory_status,
            list_facts,
            add_fact,
            update_fact,
            delete_fact,
            list_summaries,
            delete_summary,
            list_reminders,
            delete_reminder,
            clear_memory,
            export_memory,
            import_memory,
            voice_status,
            download_voice_model,
            delete_voice_model,
            answer_ask,
            voice_start,
            voice_stop
        ])
        .setup(move |app| {
            let app_handle = app.handle().clone();

            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            let db_path = data_dir.join("companion.db");
            log::info!("Database: {}", db_path.display());
            app.manage(Db::open(&db_path)?);
            app.manage(pairing::Pairing::load(&app.state::<Db>()));
            app.manage(esp32::advertise(8080, phone::PHONE_PORT));
            // Windows are `create: false` in tauri.conf.json and made here, after
            // the state above is managed. Otherwise their pages can call commands
            // before the DB exists ("state not managed for field `db`").
            for cfg in app.config().app.windows.clone() {
                tauri::WebviewWindowBuilder::from_config(app.handle(), &cfg)?.build()?;
            }
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
            tauri::async_runtime::spawn(proactive::run(app_handle.clone()));
            tauri::async_runtime::spawn(desktop::pass_through_loop(app_handle.clone()));
            tauri::async_runtime::spawn(voice::idle_reaper(app_handle.clone()));
            voice::register_shortcut(&app_handle);


            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state = app_handle.state::<AppState>();
                state.shutting_down.store(true, Ordering::Relaxed);
                voice::kill_server_now(app_handle);
                if let Some(child) = state.ollama_child.lock().unwrap().take() {
                    let _ = child.kill();
                    log::info!("Ollama sidecar killed on exit");
                };
            }
        });
}
