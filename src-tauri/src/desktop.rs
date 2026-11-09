//! Desktop presence: tray icon, the control panel window ("main"), and the
//! floating pet window ("pet"). Closing either window only hides it; Quit in
//! the tray is the only way out.

use crate::db::Db;
use crate::settings::Settings;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, Wry};

const POSITION_KEY: &str = "pet_position";

struct TrayItems {
    pet: CheckMenuItem<Wry>,
    click_through: CheckMenuItem<Wry>,
    pause: CheckMenuItem<Wry>,
}

#[derive(Default)]
pub struct PetState {
    click_through: AtomicBool,
    /// Bumped on every move, so only the last move of a drag gets saved
    move_gen: AtomicU64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
struct Position {
    x: i32,
    y: i32,
}

// ---- Control panel ----

pub fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        w.unminimize().ok();
        w.show().ok();
        w.set_focus().ok();
    }
}

// ---- Pet ----

pub fn pet_visible(app: &AppHandle) -> bool {
    app.get_webview_window("pet")
        .and_then(|w| w.is_visible().ok())
        .unwrap_or(false)
}

pub fn set_pet_visible(app: &AppHandle, visible: bool) -> Result<(), String> {
    let w = app.get_webview_window("pet").ok_or("Pet window is missing")?;
    if visible {
        w.show().map_err(|e| e.to_string())?;
    } else {
        w.hide().map_err(|e| e.to_string())?;
    }
    let db = app.state::<Db>();
    let mut s = Settings::load(&db);
    if s.pet_visible != visible {
        s.pet_visible = visible;
        s.save(&db)?;
        app.emit("settings:changed", &s).ok();
    }
    sync_tray(app);
    Ok(())
}

pub fn click_through(app: &AppHandle) -> bool {
    app.state::<PetState>().click_through.load(Ordering::Relaxed)
}

/// Lets clicks pass through the pet to whatever is under it. It can't be
/// dragged or clicked while this is on, so it's only switched off from the tray
/// or the panel. Not saved, so a restart always brings back a clickable pet.
pub fn set_click_through(app: &AppHandle, on: bool) -> Result<(), String> {
    let w = app.get_webview_window("pet").ok_or("Pet window is missing")?;
    w.set_ignore_cursor_events(on).map_err(|e| e.to_string())?;
    app.state::<PetState>().click_through.store(on, Ordering::Relaxed);
    app.emit("pet:click_through", on).ok();
    sync_tray(app);
    Ok(())
}

/// Puts the pet where it was last left, or bottom-right of the main screen if
/// that spot is no longer on any monitor (e.g. a second screen was unplugged).
pub fn place_pet(app: &AppHandle) {
    let Some(w) = app.get_webview_window("pet") else {
        return;
    };
    let saved = app
        .state::<Db>()
        .get_setting(POSITION_KEY)
        .ok()
        .flatten()
        .and_then(|j| serde_json::from_str::<Position>(&j).ok());
    let monitors = w.available_monitors().unwrap_or_default();
    let on_screen = |p: &Position| {
        monitors.iter().any(|m| {
            let (mp, ms) = (m.position(), m.size());
            // At least a 60px corner of the pet must be on this monitor
            p.x >= mp.x - 60
                && p.y >= mp.y
                && p.x < mp.x + ms.width as i32 - 60
                && p.y < mp.y + ms.height as i32 - 60
        })
    };
    let pos = saved.filter(on_screen).or_else(|| {
        let m = w.primary_monitor().ok().flatten()?;
        let size = w.outer_size().ok()?;
        let area = m.work_area();
        Some(Position {
            x: area.position.x + area.size.width as i32 - size.width as i32 - 24,
            y: area.position.y + area.size.height as i32 - size.height as i32 - 8,
        })
    });
    if let Some(p) = pos {
        w.set_position(PhysicalPosition::new(p.x, p.y)).ok();
    }
}

/// Called for every pet `Moved` event. Saves the position 600ms after the drag stops.
pub fn pet_moved(app: &AppHandle, pos: PhysicalPosition<i32>) {
    let gen = app.state::<PetState>().move_gen.fetch_add(1, Ordering::Relaxed) + 1;
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        if app.state::<PetState>().move_gen.load(Ordering::Relaxed) != gen {
            return;
        }
        let json = serde_json::to_string(&Position { x: pos.x, y: pos.y }).unwrap_or_default();
        if let Err(e) = app.state::<Db>().set_setting(POSITION_KEY, &json) {
            log::warn!("Couldn't save pet position: {e}");
        }
    });
}

// ---- Tray ----

pub fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open panel", true, None::<&str>)?;
    let pet = CheckMenuItem::with_id(app, "pet", "Show pet", true, pet_visible(app), None::<&str>)?;
    let click_through = CheckMenuItem::with_id(
        app,
        "click_through",
        "Click-through pet",
        true,
        false,
        None::<&str>,
    )?;
    let pause = CheckMenuItem::with_id(app, "pause", "Pause", true, false, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &open,
            &PredefinedMenuItem::separator(app)?,
            &pet,
            &click_through,
            &pause,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )?;

    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("diy-waifu")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let result = match event.id().as_ref() {
                "open" => {
                    show_main(app);
                    Ok(())
                }
                "pet" => set_pet_visible(app, !pet_visible(app)),
                "click_through" => set_click_through(app, !self::click_through(app)),
                "pause" => {
                    crate::pause::set_manual(app, !crate::pause::get(app).manual);
                    Ok(())
                }
                "quit" => {
                    app.exit(0);
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(e) = result {
                log::warn!("Tray action failed: {e}");
            }
            // Check items flip themselves on click; put them back to the real state
            sync_tray(app);
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;

    app.manage(TrayItems {
        pet,
        click_through,
        pause,
    });
    Ok(())
}

/// Makes the tray's check marks match the real state.
pub fn sync_tray(app: &AppHandle) {
    let Some(items) = app.try_state::<TrayItems>() else {
        return;
    };
    items.pet.set_checked(pet_visible(app)).ok();
    items.click_through.set_checked(click_through(app)).ok();
    items.pause.set_checked(crate::pause::get(app).manual).ok();
}
