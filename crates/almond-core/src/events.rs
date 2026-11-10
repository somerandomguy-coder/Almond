//! Server → client notifications (docs/API.md "Notifications"). Transports subscribe and forward:
//! the Tauri shell emits them as events named with '.' → '_'.

use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

#[derive(Debug, Clone)]
pub struct Notification {
    /// e.g. `almond_types::api::notifications::JOB_PROGRESS`
    pub name: &'static str,
    pub payload: Value,
}

#[derive(Clone)]
pub struct Notifier {
    tx: broadcast::Sender<Notification>,
}

impl Default for Notifier {
    fn default() -> Self {
        Self::new()
    }
}

impl Notifier {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Notifier { tx }
    }

    /// Send to all subscribers; silently dropped when nobody listens.
    pub fn emit(&self, name: &'static str, payload: impl Serialize) {
        match serde_json::to_value(payload) {
            Ok(payload) => {
                let _ = self.tx.send(Notification { name, payload });
            }
            Err(e) => tracing::warn!("notification {name}: {e}"),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notification> {
        self.tx.subscribe()
    }
}
