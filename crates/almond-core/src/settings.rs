//! `settings.get` / `settings.set` (local to the device, never synced) and `app.info`.
//! Keys are listed in docs/API.md ("Settings keys"); values are any JSON.

use crate::Core;
use crate::error::ApiResult;
use crate::router::{NoParams, Router};
use almond_types::Result;
use almond_types::store::Store;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Well-known settings keys.
pub mod keys {
    pub const UI_LANGUAGE: &str = "ui.language";
    pub const UI_THEME: &str = "ui.theme";
    pub const REMINDER_LEAD_MIN: &str = "calendar.reminder_lead_min";
    pub const SYNC_AUTO_WHILE_OPEN: &str = "sync.auto_while_open";
    pub const GOOGLE_OAUTH_CLIENT: &str = "google.oauth_client";
    pub const OLLAMA_URL: &str = "llm.ollama_url";
    /// A stronger machine's Ollama (e.g. the home PC over Tailscale, `http://100.x.y.z:11434`).
    /// Used instead of the local one whenever it answers; the local one is the fallback.
    pub const REMOTE_OLLAMA_URL: &str = "llm.remote_ollama_url";
    /// Job offload policy (`processing::offload`): `"local"` (default) or `"prefer_peer"`.
    pub const OFFLOAD: &str = "processing.offload";
    /// `prefer_peer`: minutes a job waits for the stronger peer before running here anyway.
    pub const OFFLOAD_WAIT_MIN: &str = "processing.offload_wait_min";
    pub const MODEL_SLOTS: &str = "models.slots";
}

fn kv_key(key: &str) -> String {
    format!("settings.{key}")
}

impl Core {
    /// Raw JSON setting, `Value::Null` if unset.
    pub fn setting(&self, key: &str) -> Result<Value> {
        Ok(match self.store().kv_get(&kv_key(key))? {
            Some(s) => serde_json::from_str(&s)?,
            None => Value::Null,
        })
    }

    /// Typed setting; `None` if unset or of another shape.
    pub fn setting_as<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        Ok(serde_json::from_value(self.setting(key)?).ok())
    }

    pub fn set_setting(&self, key: &str, value: &Value) -> Result<()> {
        self.store().kv_set(&kv_key(key), &value.to_string())
    }

    /// Effective Ollama URL: setting `llm.ollama_url`, else the config default.
    pub fn ollama_url(&self) -> String {
        self.setting_as::<String>(keys::OLLAMA_URL)
            .ok()
            .flatten()
            .unwrap_or_else(|| self.cfg.ollama_url.clone())
    }
}

#[derive(Deserialize)]
struct GetParams {
    key: String,
}

#[derive(Deserialize)]
struct SetParams {
    key: String,
    value: Value,
}

#[derive(Serialize)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: String,
    pub api_version: u32,
    pub platform: String,
    pub device_id: String,
    pub device_name: String,
}

pub(crate) fn register(r: &mut Router) {
    r.add("settings.get", |core, p: GetParams| async move {
        ApiResult::Ok(core.setting(&p.key)?)
    });
    r.add("settings.set", |core, p: SetParams| async move {
        core.set_setting(&p.key, &p.value)?;
        ApiResult::Ok(Value::Null)
    });
    r.add("app.info", |core, _: NoParams| async move {
        ApiResult::Ok(AppInfo {
            name: "Almond",
            version: core.cfg.app_version.clone(),
            api_version: almond_types::api::API_VERSION,
            platform: core.cfg.platform.clone(),
            device_id: core.device_id().0.clone(),
            device_name: core.device().name.clone(),
        })
    });
}
