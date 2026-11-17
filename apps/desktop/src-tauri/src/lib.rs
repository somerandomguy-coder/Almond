//! Almond Tauri shell: opens `almond-core` and routes every UI call to it.
//!
//! Conventions (docs/API.md): the UI invokes `invoke("meetings_list", { params })`; the command
//! name is the API method with the first '.' replaced by '_' (`notes_applyUpdate` →
//! `notes.applyUpdate`). Core notifications are emitted as Tauri events with '.' → '_'.
//! Errors reject with `{ code, message }`.

use almond_core::{Api, Core, CoreConfig};
use serde_json::Value;
use tauri::ipc::{Invoke, InvokeBody, InvokeError};
use tauri::{Emitter, Manager};

/// `meetings_list` → `meetings.list`, `notes_applyUpdate` → `notes.applyUpdate`, `app_info` → `app.info`.
fn method_name(command: &str) -> String {
    command.replacen('_', ".", 1)
}

fn device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .and_then(|n| (!n.trim().is_empty()).then(|| n.trim().to_string()))
        .unwrap_or_else(|| format!("Almond ({})", std::env::consts::OS))
}

/// Where meetings live: `ALMOND_DATA_DIR`, else `~/.almond/data` on desktop (next to the speech
/// models), else the app's data folder (Android).
///
/// Not `%APPDATA%` on Windows: apps started from inside a packaged (MSIX) app, such as a dev
/// build run from a terminal inside one, get their AppData writes redirected to a private copy,
/// so a dev run and the installed app saw different data, and a database and its journal ended up
/// in different places (one stale), which looked like corruption. The home folder isn't
/// redirected.
fn data_dir(app: &tauri::App) -> tauri::Result<std::path::PathBuf> {
    if let Some(d) = std::env::var_os("ALMOND_DATA_DIR").filter(|d| !d.is_empty()) {
        return Ok(d.into());
    }
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return app.path().app_data_dir();
    }
    Ok(app.path().home_dir()?.join(".almond").join("data"))
}

fn handle(invoke: Invoke) -> bool {
    let api = invoke.message.webview().state::<Api>().inner().clone();
    let method = method_name(invoke.message.command());
    let params = match invoke.message.payload() {
        InvokeBody::Json(v) => v.get("params").cloned().unwrap_or(Value::Null),
        InvokeBody::Raw(_) => Value::Null,
    };
    invoke
        .resolver
        .respond_async(async move { api.call(&method, params).await.map_err(InvokeError::from) });
    true
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let data_dir = data_dir(app)?;
            let mut cfg = CoreConfig::new(data_dir, device_name());
            // Opened from the Start menu: start Ollama too if it isn't running.
            cfg.start_ollama = true;
            // Data used to live in the app's AppData folder; its device key is adopted.
            if let Ok(old) = app.path().app_data_dir() {
                cfg.previous_data_dirs.push(old);
            }
            let api = tauri::async_runtime::block_on(Core::open(cfg))?;

            // forward core notifications to the UI
            let mut rx = api.subscribe();
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(n) => {
                            let _ = handle.emit(&n.name.replace('.', "_"), n.payload);
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(_) => break,
                    }
                }
            });
            app.manage(api);
            Ok(())
        })
        .invoke_handler(handle)
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event
                && let Some(api) = app.try_state::<Api>()
            {
                tauri::async_runtime::block_on(api.core.shutdown());
            }
        });
}

#[cfg(test)]
mod tests {
    use super::method_name;

    #[test]
    fn command_to_method() {
        assert_eq!(method_name("meetings_list"), "meetings.list");
        assert_eq!(method_name("notes_applyUpdate"), "notes.applyUpdate");
        assert_eq!(method_name("app_info"), "app.info");
    }
}
