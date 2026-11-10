//! # almond-core — the app's back end
//!
//! Owns every subsystem (store + sync node, processor registry, audio/STT, LLM, calendar, verify)
//! and exposes them through one method router (docs/API.md). Transports call [`Api::call`] and
//! forward [`Notifier`] notifications: the Tauri shell today, JSON-RPC over WebSocket and MCP later.
//!
//! ## Layout (Phase 2 ownership)
//!
//! | Module | Owner | Methods |
//! |---|---|---|
//! | `lib.rs`, `router.rs`, `error.rs`, `events.rs`, `settings.rs` | integrator | `settings.*`, `app.info` |
//! | [`capture`] | Agent "capture" | `recording.*`, `audio.*`, live captions, `transcribe.*` processor, `models.*` (speech) |
//! | [`processing`] | Agent "processing" | `artifacts.*`, `jobs.*`, `pipeline.*`, `verify.*`, `actions.*`, job runner, LLM + verify processors, `models.*` (LLM) |
//! | [`library`] | Agent "library" | `meetings.*`, `notes.*`, `search.*`, `calendar.*`, `mail.*`, `sync.*`, `storage.*`, reminders, auto-sync |
//!
//! Each module keeps its own state in a `State` struct (a field of [`Core`]) and has:
//! - `State::init(&Shared) -> Result<State>` — build, called once by [`Core::open`];
//! - `register(&mut Router)` — add its methods;
//! - `start(Arc<Core>)` — spawn background tasks (job runner loop, reminders, ...), called once
//!   after the `Core` exists.
//!
//! Modules talk to each other only through `Core`'s public methods/fields and the store's event log.

pub mod capture;
pub mod error;
pub mod events;
pub mod library;
pub mod processing;
pub mod router;
pub mod settings;

pub use almond_types as types;
pub use error::{ApiError, ApiResult};
pub use events::{Notification, Notifier};
pub use router::Router;

use almond_pipeline::ProcessorRegistry;
use almond_store::{FsBlobStore, SqliteStore};
use almond_sync::{NodeConfig, SyncNode};
use almond_types::job::Capability;
use almond_types::sync::DeviceInfo;
use almond_types::{DeviceId, Result};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// App data: `almond.db`, `blobs/`, device key, recordings in progress.
    pub data_dir: PathBuf,
    /// Speech models (shared with the eval tools): default `~/.almond/models`.
    pub models_dir: PathBuf,
    /// User-facing device name, e.g. "Nam's PC".
    pub device_name: String,
    /// "windows" | "linux" | "android"
    pub platform: String,
    pub app_version: String,
    /// What this device can run (`gpu`, `llm`, `llm:<model>`, `stt:vi`, ...). See `Capability`.
    pub capabilities: Vec<Capability>,
    /// Ollama base URL; setting `llm.ollama_url` overrides it at runtime.
    pub ollama_url: String,
    /// `false` binds sync to localhost on a random port without mDNS (tests, offline use).
    pub network: bool,
}

impl CoreConfig {
    pub fn new(data_dir: impl Into<PathBuf>, device_name: impl Into<String>) -> Self {
        CoreConfig {
            data_dir: data_dir.into(),
            models_dir: almond_stt::models::default_models_dir(),
            device_name: device_name.into(),
            platform: std::env::consts::OS.to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            capabilities: vec![],
            ollama_url: std::env::var("OLLAMA_HOST")
                .ok()
                .map(|h| {
                    if h.starts_with("http") {
                        h
                    } else {
                        format!("http://{h}")
                    }
                })
                .unwrap_or_else(|| "http://127.0.0.1:11434".into()),
            network: true,
        }
    }
}

/// Everything the modules share. Built first; modules' `State::init` receive it.
pub struct Shared {
    pub cfg: CoreConfig,
    pub node: SyncNode,
    pub notifier: Notifier,
    /// Processors this device knows (and can run if capabilities allow). Modules register theirs
    /// in `State::init`; the processing module's job runner executes them.
    pub registry: Arc<ProcessorRegistry>,
}

impl Shared {
    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.node.store
    }
    pub fn blobs(&self) -> &Arc<FsBlobStore> {
        &self.node.blobs
    }
    pub fn device(&self) -> &DeviceInfo {
        &self.node.device
    }
    pub fn device_id(&self) -> &DeviceId {
        &self.node.device.id
    }
}

pub struct Core {
    pub shared: Shared,
    pub capture: capture::State,
    pub processing: processing::State,
    pub library: library::State,
}

impl std::ops::Deref for Core {
    type Target = Shared;
    fn deref(&self) -> &Shared {
        &self.shared
    }
}

impl Core {
    /// Open the store, sync node and every module, then start background tasks.
    pub async fn open(cfg: CoreConfig) -> Result<Api> {
        let mut node_cfg = NodeConfig::new(&cfg.data_dir, &cfg.device_name);
        node_cfg.platform = cfg.platform.clone();
        node_cfg.app_version = cfg.app_version.clone();
        node_cfg.capabilities = cfg.capabilities.clone();
        if !cfg.network {
            node_cfg.iroh = almond_sync::IrohConfig {
                port: 0,
                bind_ip: Some(std::net::Ipv4Addr::LOCALHOST.into()),
                mdns: false,
            };
        }
        let node = SyncNode::open(node_cfg).await?;
        let shared = Shared {
            cfg,
            node,
            notifier: Notifier::new(),
            registry: Arc::new(ProcessorRegistry::new()),
        };
        let capture = capture::State::init(&shared).await?;
        let processing = processing::State::init(&shared).await?;
        let library = library::State::init(&shared).await?;
        let core = Arc::new(Core {
            shared,
            capture,
            processing,
            library,
        });

        let mut router = Router::default();
        settings::register(&mut router);
        capture::register(&mut router);
        processing::register(&mut router);
        library::register(&mut router);

        capture::start(core.clone());
        processing::start(core.clone());
        library::start(core.clone());

        Ok(Api {
            core,
            router: Arc::new(router),
        })
    }

    pub async fn shutdown(&self) {
        self.capture.shutdown().await;
        self.processing.shutdown().await;
        self.library.shutdown().await;
        self.node.shutdown().await;
    }
}

/// Handle used by transports: route a method call, subscribe to notifications.
#[derive(Clone)]
pub struct Api {
    pub core: Arc<Core>,
    router: Arc<Router>,
}

impl Api {
    pub async fn call(&self, method: &str, params: Value) -> ApiResult<Value> {
        self.router.call(self.core.clone(), method, params).await
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<Notification> {
        self.core.notifier.subscribe()
    }

    /// Methods with a handler (the rest return "not implemented").
    pub fn methods(&self) -> Vec<String> {
        self.router.methods().map(str::to_string).collect()
    }
}
