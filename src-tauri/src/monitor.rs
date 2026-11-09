//! Every 5s, auto-pauses the companion while a heavy app (from settings) is
//! running, so the model's RAM and CPU go to that app instead.
//! Phase 3 replaces this with the foreground-app tracker in activity.rs.

use crate::db::Db;
use crate::settings::Settings;
use sysinfo::System;
use tauri::Manager;

/// The first heavy app in `running` (process names), if any.
fn find_heavy(running: impl IntoIterator<Item = String>, heavy: &[String]) -> Option<&str> {
    running.into_iter().find_map(|name| {
        let name = name.to_lowercase();
        heavy.iter().find(|h| **h == name).map(String::as_str)
    })
}

pub async fn run_monitor_loop(app: tauri::AppHandle) {
    let mut sys = System::new();

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        let settings = Settings::load(&app.state::<Db>());

        let reason = if settings.auto_pause && !settings.heavy_apps.is_empty() {
            sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            let names = sys
                .processes()
                .values()
                .map(|p| p.name().to_string_lossy().into_owned());
            find_heavy(names, &settings.heavy_apps).map(|app| format!("{app} is running"))
        } else {
            None
        };
        // No-op unless it changed
        crate::pause::set_auto(&app, reason);
    }
}

#[cfg(test)]
mod tests {
    use super::find_heavy;

    #[test]
    fn matches_process_names_case_insensitively() {
        let heavy = vec!["blender.exe".to_string()];
        let running = ["explorer.exe", "Blender.exe"].map(String::from);
        assert_eq!(find_heavy(running, &heavy), Some("blender.exe"));
        assert_eq!(find_heavy(["code.exe".to_string()], &heavy), None);
    }
}
