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
        }
    }
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
        let identity = DeviceIdentity::load_or_create(secrets.as_ref())?;
        let device = DeviceInfo {
            id: identity.device_id(),
            name: cfg.name.clone(),
            platform: cfg.platform.clone(),
            app_version: cfg.app_version.clone(),
            capabilities: cfg.capabilities.clone(),
        };
        let store = Arc::new(SqliteStore::open(
            cfg.data_dir.join("almond.db"),
            device.id.clone(),
        )?);
        let blobs = Arc::new(FsBlobStore::open(cfg.data_dir.join("blobs"))?);

        // announce (changes to) this device to the vault
        let known = store.devices()?.into_iter().find(|d| d.id == device.id);
        if known.as_ref() != Some(&device) {
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
