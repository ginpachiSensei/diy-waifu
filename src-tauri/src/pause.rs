//! Pausing. The companion is paused if you paused it (manual) or a heavy app
//! is running (auto). Each is tracked on its own, so closing Blender never
//! un-pauses something you paused by hand. While paused, chat is turned away and
//! the model is unloaded to free RAM.

use crate::db::Db;
use crate::settings::Settings;
use crate::AppState;
use serde::Serialize;
use std::sync::atomic::Ordering;
use tauri::{Emitter, Manager};

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PauseState {
    pub paused: bool,
    pub manual: bool,
    /// Set while auto-paused, e.g. "blender.exe is running"
    pub auto_reason: Option<String>,
}

impl PauseState {
    fn recompute(&mut self) {
        self.paused = self.manual || self.auto_reason.is_some();
    }
}

pub fn get(app: &tauri::AppHandle) -> PauseState {
    app.state::<AppState>().pause.lock().unwrap().clone()
}

pub fn set_manual(app: &tauri::AppHandle, manual: bool) {
    update(app, |s| s.manual = manual);
}

pub fn set_auto(app: &tauri::AppHandle, reason: Option<String>) {
    update(app, |s| s.auto_reason = reason);
}

fn update(app: &tauri::AppHandle, f: impl FnOnce(&mut PauseState)) {
    let state = app.state::<AppState>();
    let (before, after) = {
        let mut s = state.pause.lock().unwrap();
        let before = s.clone();
        f(&mut s);
        s.recompute();
        (before, s.clone())
    };
    if before == after {
        return;
    }
    state.paused.store(after.paused, Ordering::Relaxed);
    log::info!("Pause state: {:?}", after);
    app.emit("companion:paused", &after).ok();
    crate::desktop::sync_tray(app);

    if after.paused && !before.paused {
        let model = Settings::load(&app.state::<Db>()).model;
        tauri::async_runtime::spawn(async move {
            crate::ollama::unload_model(&model).await.ok();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::PauseState;

    #[test]
    fn manual_pause_survives_auto_resume() {
        let mut s = PauseState {
            manual: true,
            auto_reason: Some("blender.exe is running".into()),
            ..Default::default()
        };
        s.recompute();
        assert!(s.paused);
        s.auto_reason = None; // Blender closed
        s.recompute();
        assert!(s.paused);
        s.manual = false;
        s.recompute();
        assert!(!s.paused);
    }
}
