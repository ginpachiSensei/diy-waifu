// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod websocket;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .setup(|app| {
            // Start the WebSocket server on app launch
            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async move {
                    if let Err(e) = websocket::start_server(app_handle, "0.0.0.0".to_string(), 8080).await {
                        eprintln!("WebSocket server error: {}", e);
                    }
                });
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
