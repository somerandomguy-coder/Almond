//! [`SyncNode`]: one-call setup of identity + store + blobs + iroh transport + sync engine for a
//! data directory. This is what `almond-core` / the Tauri app is expected to hold while open.

use crate::engine::{SyncEngine, SyncOptions};
use crate::identity::DeviceIdentity;
use crate::iroh_transport::{IrohConfig, IrohTransport};
use crate::peers::PeerBook;
use crate::secrets::default_secret_store;
use almond_store::{FsBlobStore, SqliteStore};
use almond_types::Result;
use almond_types::event::EventBody;
use almond_types::job::Capability;
use almond_types::secrets::SecretStore;
use almond_types::store::Store;
use almond_types::sync::{DeviceInfo, Transport};
use std::path::PathBuf;
use std::sync::Arc;

pub struct NodeConfig {
    pub data_dir: PathBuf,
    /// User-facing device name, e.g. "Nam's PC".
    pub name: String,
    /// "windows" | "linux" | "android"
    pub platform: String,
    pub app_version: String,
    pub capabilities: Vec<Capability>,
    pub iroh: IrohConfig,
    pub sync: SyncOptions,
    /// Defaults to [`default_secret_store`] in `data_dir`.
    pub secrets: Option<Arc<dyn SecretStore>>,
    /// Folders this data used to live in. The device key is stored per folder path, so after the
    /// data moves, the key that owns the database is found under one of these and adopted.
    pub previous_data_dirs: Vec<PathBuf>,
}

impl NodeConfig {
    pub fn new(data_dir: impl Into<PathBuf>, name: impl Into<String>) -> Self {
        NodeConfig {
            data_dir: data_dir.into(),
            name: name.into(),
            platform: std::env::consts::OS.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec![],
            iroh: IrohConfig::default(),
            sync: SyncOptions::default(),
            secrets: None,
            previous_data_dirs: vec![],
        }
    }
}

/// The database in `data_dir` belongs to a device whose key isn't this folder's (the data was
/// moved): take the owner's key from a previous folder's store. The key it replaces is kept under
/// `device_key.replaced-<unix ms>`.
fn adopt_moved_key(
    secrets: &dyn SecretStore,
    data_dir: &std::path::Path,
    previous: &[PathBuf],
) -> Result<()> {
    use almond_types::secrets::keys;
    let Some(owner) = almond_store::database_owner(&data_dir.join("almond.db")) else {
        return Ok(());
    };
    let current = secrets.get(keys::DEVICE_KEY)?;
    let id_of = |k: &[u8]| -> Option<String> {
        let arr: [u8; 32] = k.try_into().ok()?;
        Some(iroh::SecretKey::from_bytes(&arr).public().to_string())
    };
    if current.as_deref().and_then(id_of).as_deref() == Some(owner.as_str()) {
        return Ok(());
    }
    for dir in previous.iter().filter(|d| d.as_path() != data_dir) {
        let Ok(Some(k)) = default_secret_store(dir).get(keys::DEVICE_KEY) else {
            continue;
        };
        if id_of(&k).as_deref() != Some(owner.as_str()) {
            continue;
        }
        if let Some(old) = &current {
            let at = almond_types::time::now_ms();
            secrets.set(&format!("{}.replaced-{at}", keys::DEVICE_KEY), old)?;
        }
        secrets.set(keys::DEVICE_KEY, &k)?;
        tracing::warn!(
            "data moved from {}: using its device key {owner} here",
            dir.display()
        );
        return Ok(());
    }
    Ok(())
}

pub struct SyncNode {
    pub identity: DeviceIdentity,
    pub device: DeviceInfo,
    pub store: Arc<SqliteStore>,
    pub blobs: Arc<FsBlobStore>,
    pub transport: Arc<IrohTransport>,
    pub engine: Arc<SyncEngine>,
    server: tokio::task::JoinHandle<()>,
}

impl SyncNode {
    pub async fn open(cfg: NodeConfig) -> Result<Self> {
        std::fs::create_dir_all(&cfg.data_dir)?;
        let secrets = cfg
            .secrets
            .clone()
            .unwrap_or_else(|| default_secret_store(&cfg.data_dir));
        if cfg.secrets.is_none() {
            adopt_moved_key(secrets.as_ref(), &cfg.data_dir, &cfg.previous_data_dirs)?;
        }
        let identity = DeviceIdentity::load_or_create(secrets.as_ref())?;
        let device = DeviceInfo {
            id: identity.device_id(),
            name: cfg.name.clone(),
            platform: cfg.platform.clone(),
            app_version: cfg.app_version.clone(),
            capabilities: cfg.capabilities.clone(),
            last_active: None,
        };
        // A damaged database (power loss, full disk) is backed up and salvaged instead of
        // failing start-up; the report is kept under kv `store.last_recovery`.
        let (store, _recovery) =
            almond_store::open_or_recover(cfg.data_dir.join("almond.db"), device.id.clone())?;
        let store = Arc::new(store);
        let blobs = Arc::new(FsBlobStore::open(cfg.data_dir.join("blobs"))?);

        // announce (changes to) this device to the vault
        // (`last_active` is re-announced separately while the app is in use; ignore it here)
        let known = store.devices()?.into_iter().find(|d| d.id == device.id);
        let same = known.as_ref().is_some_and(|k| {
            DeviceInfo {
                last_active: None,
                ..k.clone()
            } == device
        });
        if !same {
            store.append_local(EventBody::DeviceInfo(device.clone()))?;
        }

        let book = Arc::new(PeerBook::load(store.clone() as Arc<dyn Store>)?);
        let transport =
            Arc::new(IrohTransport::bind(&identity, device.clone(), book, cfg.iroh.clone()).await?);
        let engine = SyncEngine::new(
            store.clone(),
            blobs.clone(),
            transport.clone() as Arc<dyn Transport>,
            device.clone(),
            cfg.sync.clone(),
        );
        let server = engine.spawn_server();
        Ok(SyncNode {
            identity,
            device,
            store,
            blobs,
            transport,
            engine,
            server,
        })
    }

    pub async fn shutdown(&self) {
        self.server.abort();
        self.transport.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::adopt_moved_key;
    use crate::identity::DeviceIdentity;
    use crate::secrets::default_secret_store;
    use almond_types::secrets::keys;

    #[test]
    fn a_moved_database_keeps_its_device_key() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // Folder A: the device key and a database it owns.
        let id_a = DeviceIdentity::load_or_create(default_secret_store(a.path()).as_ref()).unwrap();
        drop(
            almond_store::SqliteStore::open(a.path().join("almond.db"), id_a.device_id()).unwrap(),
        );
        // The data moves to B, where a fresh key was already made by mistake.
        std::fs::copy(a.path().join("almond.db"), b.path().join("almond.db")).unwrap();
        let sb = default_secret_store(b.path());
        let stray = DeviceIdentity::load_or_create(sb.as_ref()).unwrap();
        assert_ne!(stray.device_id(), id_a.device_id());

        adopt_moved_key(sb.as_ref(), b.path(), &[a.path().to_path_buf()]).unwrap();
        assert_eq!(
            DeviceIdentity::load_or_create(sb.as_ref())
                .unwrap()
                .device_id(),
            id_a.device_id()
        );
        // Idempotent, and nothing to do without a previous folder holding the owner's key.
        adopt_moved_key(sb.as_ref(), b.path(), &[]).unwrap();
        assert_eq!(
            DeviceIdentity::load_or_create(sb.as_ref())
                .unwrap()
                .device_id(),
            id_a.device_id()
        );
        assert!(sb.get(keys::DEVICE_KEY).unwrap().is_some());
    }
}
