//! Almond Tauri shell. Phase 0: one real command proving the Rust ↔ UI bridge.
//! Phase 2 (integration) wires `almond-core` here and exposes every method in docs/API.md as a
//! command named like the method with '.' → '_' (e.g. `meetings_list`).

use serde::Serialize;

#[derive(Serialize)]
struct AppInfo {
    name: &'static str,
    version: &'static str,
    api_version: u32,
    platform: &'static str,
}

#[tauri::command]
fn app_info() -> AppInfo {
    AppInfo {
        name: "Almond",
        version: env!("CARGO_PKG_VERSION"),
        api_version: almond_types::api::API_VERSION,
        platform: std::env::consts::OS,
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![app_info])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
