use sysinfo::System;

const HEAVY_APPS: &[&str] = &["blender.exe", "unity.exe", "unityhub.exe"];

pub async fn run_monitor_loop(app_handle: tauri::AppHandle) {
    let mut sys = System::new_all();
    let mut currently_paused = false;

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

        let heavy_app_running = sys.processes().values().any(|p| {
            let name = p.name().to_string_lossy().to_lowercase();
            HEAVY_APPS.iter().any(|h| name == *h)
        });

        if heavy_app_running && !currently_paused {
            println!("Heavy app detected — pausing companion");
            currently_paused = true;
            app_handle.emit("companion:paused", true).ok();
            // TODO: actually unload the Ollama model here via
            // POST /api/generate with "keep_alive": 0, or skip chat calls entirely
        } else if !heavy_app_running && currently_paused {
            println!("Heavy app closed — resuming companion");
            currently_paused = false;
            app_handle.emit("companion:paused", false).ok();
        }
    }
}
