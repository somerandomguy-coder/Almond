//! Phase 2 module "library" (see the ownership table in lib.rs and the brief in docs/PHASE2.md):
//! meetings, notes, search, calendar sources + reminders, Gmail context, LAN sync and storage
//! usage, all on top of the store and the sync node that [`Shared`] already holds.
//!
//! | file | methods / tasks |
//! |---|---|
//! | [`meetings`] | `meetings.*`, `notes.*`, `search.query`, `storage.usage`, [`notes_text`] |
//! | [`calendar`] | `calendar.*`, the poll loop (`PollPolicy`), the reminder loop (`calendar.reminder`) |
//! | [`mail`] | `mail.context` |
//! | [`sync`] | `sync.*`, auto-sync while open, `device.info` activity heartbeats |
//!
//! Every method registered here counts as user activity on this device ([`touch`]): it feeds
//! the reminder policy ("only the most recently used device notifies") and is re-announced to
//! peers as `DeviceInfo::last_active` at most every few minutes.

mod calendar;
mod mail;
mod meetings;
mod sync;

pub use calendar::{push_phone_snapshot, set_url_opener};
pub use meetings::{StorageUsage, notes_text};

use crate::error::ApiResult;
use crate::router::Router;
use crate::{Core, Shared};
use almond_calendar::DeviceActivity;
use almond_types::Result;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

pub struct State {
    pub(crate) activity: Arc<DeviceActivity>,
    pub(crate) cal: calendar::CalState,
    pub(crate) sync: sync::SyncState,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl State {
    pub(crate) async fn init(shared: &Shared) -> Result<State> {
        // Same store the sync node uses for the device key (scoped to the data dir).
        let secrets = almond_sync::secrets::default_secret_store(&shared.cfg.data_dir);
        let activity = Arc::new(DeviceActivity::new());
        let cal = calendar::CalState::new(shared, secrets, activity.clone())?;
        Ok(State {
            activity,
            cal,
            sync: sync::SyncState::default(),
            tasks: Mutex::new(vec![]),
        })
    }

    pub(crate) async fn shutdown(&self) {
        for t in self
            .tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .drain(..)
        {
            t.abort();
        }
    }

    fn spawn(&self, f: impl Future<Output = ()> + Send + 'static) {
        self.tasks
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(tokio::spawn(f));
    }
}

pub(crate) fn register(r: &mut Router) {
    meetings::register(r);
    calendar::register(r);
    mail::register(r);
    sync::register(r);
}

pub(crate) fn start(core: Arc<Core>) {
    let lib = &core.library;
    lib.spawn(calendar::poll_loop(core.clone()));
    lib.spawn(calendar::reminder_loop(core.clone()));
    lib.spawn(sync::auto_sync_loop(core.clone()));
}

/// Record user activity on this device (see the module docs).
pub fn touch(core: &Core) {
    sync::touch(core);
}

/// `Router::add` + [`touch`] on every call.
fn add<P, R, F, Fut>(r: &mut Router, method: &str, f: F)
where
    P: DeserializeOwned + Send + 'static,
    R: Serialize + 'static,
    F: Fn(Arc<Core>, P) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ApiResult<R>> + Send + 'static,
{
    r.add(method, move |core: Arc<Core>, p: P| {
        touch(&core);
        f(core, p)
    });
}
