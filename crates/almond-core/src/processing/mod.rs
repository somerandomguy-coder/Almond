//! Phase 2 module "processing": the job loop, LLM + verify processors, actions and model
//! selection (docs/PHASE2.md, "Module `processing`").
//!
//! - [`jobloop`]: watches the store's event log (local writes and synced events), emits
//!   `artifact.added`, plans jobs for every meeting that changed and runs what this device can.
//! - [`procs`]: the LLM processors (`summarize.llm`, `enhance_notes.llm`, `action_items.llm`,
//!   `title.llm`) and `verify.all`, which read the model slots, the Ollama URL, the user's notes
//!   and the linked calendar event at run time; `verify.extract_claims` (rules).
//! - [`caps`]: capability detection (Ollama models, NVIDIA GPU, speech models) and model slots.
//! - [`actions`]: post-meeting actions (`export.markdown`).
//! - [`api`]: the `artifacts.*`, `jobs.*`, `pipeline.*`, `verify.*`, `actions.*`, `models.*`
//!   methods.

mod actions;
mod api;
pub mod caps;
mod jobloop;
pub(crate) mod offload;
mod procs;
mod stubs;

use crate::router::Router;
use crate::{Core, Shared};
use almond_llm::{Ollama, OllamaConfig};
use almond_pipeline::{JobRunner, PlanPolicy, RunnerConfig};
use almond_types::api::JobProgress;
use almond_types::llm::LlmProvider;
use almond_types::store::{BlobStore, Store};
use almond_types::time::HlcClock;
use almond_types::{Error, Id, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;
use tokio::sync::Notify;

pub use caps::{CapsInfo, SLOT_LLM_SUMMARY, SLOT_LLM_VERIFY};

/// Late-bound handle to the `Core` for processors registered in `State::init` (before the `Core`
/// exists). Set in [`start`].
#[derive(Clone, Default)]
pub(crate) struct CoreRef(Arc<OnceLock<Weak<Core>>>);

impl CoreRef {
    pub(crate) fn get(&self) -> Result<Arc<Core>> {
        self.0
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| Error::Other("core is not running".into()))
    }
}

/// Where LLM calls go: the configured Ollama (cached per URL) unless overridden.
#[derive(Default)]
struct LlmSource {
    override_: Option<Arc<dyn LlmProvider>>,
    ollama: Option<(String, Arc<dyn LlmProvider>)>,
}

pub struct State {
    pub(crate) runner: Arc<JobRunner>,
    pub(crate) core: CoreRef,
    /// Wakes the job loop (new local work, API calls).
    pub(crate) wake: Arc<Notify>,
    llm: Mutex<LlmSource>,
    /// The remote Ollama (setting `llm.remote_ollama_url`) when it answered the last probe.
    remote_live: Mutex<Option<String>>,
    /// When paired peers were last reachable (offload policy).
    pub(crate) reach: offload::Reachability,
    /// Serializes capability refreshes so the latest one wins.
    caps_lock: tokio::sync::Mutex<()>,
    pub(crate) caps: Mutex<CapsInfo>,
    /// Last live progress per job (for `jobs.list`).
    pub(crate) progress: Mutex<HashMap<Id, JobProgress>>,
    /// Serializes planning so two triggers never request the same job twice.
    pub(crate) plan_lock: Mutex<()>,
    pub(crate) clock: HlcClock,
    pub(crate) actions: actions::Actions,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl State {
    pub(crate) async fn init(shared: &Shared) -> Result<State> {
        let core = CoreRef::default();
        procs::register(shared, &core)?;
        let runner = Arc::new(JobRunner::new(
            shared.store().clone() as Arc<dyn Store>,
            shared.registry.clone(),
            shared.blobs().clone() as Arc<dyn BlobStore>,
            shared.cfg.capabilities.clone(),
            RunnerConfig {
                // The job loop plans from the event log (one planner for local and synced work).
                auto_plan: false,
                // LLM calls on a shared GPU can be slow; renew often, expire after 10 min silence.
                lease_ms: 10 * 60_000,
                retry_backoff: Duration::from_secs(3),
                // Transcript first, then summary, title, action items, notes; the fact-check
                // (one LLM call per claim, the slowest step) runs last so it never delays what
                // the user reads first. Claim extraction is rule-based and instant.
                policy: PlanPolicy {
                    priorities: [
                        ("transcribe.local", 5),
                        ("summarize.llm", 4),
                        ("verify.extract_claims", 4),
                        ("title.llm", 3),
                        ("action_items.llm", 2),
                        ("enhance_notes.llm", 1),
                        ("verify.all", 0),
                    ]
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
                    ..Default::default()
                },
                ..Default::default()
            },
        ));
        Ok(State {
            runner,
            core,
            wake: Arc::new(Notify::new()),
            llm: Mutex::new(LlmSource::default()),
            remote_live: Mutex::new(None),
            reach: Default::default(),
            caps_lock: tokio::sync::Mutex::new(()),
            caps: Mutex::new(CapsInfo::default()),
            progress: Mutex::new(HashMap::new()),
            plan_lock: Mutex::new(()),
            clock: HlcClock::new(),
            actions: actions::Actions::new(&shared.cfg),
            tasks: Mutex::new(vec![]),
        })
    }

    pub(crate) async fn shutdown(&self) {
        for t in self.tasks.lock().unwrap().drain(..) {
            t.abort();
        }
    }

    /// The job runner (claims, leases, cancel) shared by the loop and the API.
    pub fn runner(&self) -> &Arc<JobRunner> {
        &self.runner
    }

    /// Capabilities and LLM models detected at the last refresh.
    pub fn capabilities(&self) -> CapsInfo {
        self.caps.lock().unwrap().clone()
    }

    /// Wake the job loop now (e.g. after writing a meeting's audio).
    pub fn kick(&self) {
        self.wake.notify_one();
    }

    /// Use `llm` instead of Ollama for every LLM call on this device (tests, a future remote
    /// provider), then re-detect capabilities.
    pub async fn set_llm_provider(&self, core: &Core, llm: Arc<dyn LlmProvider>) {
        self.llm.lock().unwrap().override_ = Some(llm);
        self.refresh_capabilities(core).await;
    }

    /// The LLM provider for this device: the override, else Ollama at `core.ollama_url()`.
    pub fn llm_provider(&self, core: &Core) -> Result<Arc<dyn LlmProvider>> {
        let mut src = self.llm.lock().unwrap();
        if let Some(p) = &src.override_ {
            return Ok(p.clone());
        }
        let url = ollama_url(core);
        if let Some((u, p)) = &src.ollama
            && *u == url
        {
            return Ok(p.clone());
        }
        let p: Arc<dyn LlmProvider> = Arc::new(Ollama::new(OllamaConfig {
            base_url: url.clone(),
            ..OllamaConfig::from_env()
        })?);
        src.ollama = Some((url, p.clone()));
        Ok(p)
    }

    /// Re-detect capabilities (Ollama models, GPU, speech models) and apply them to the runner.
    pub async fn refresh_capabilities(&self, core: &Core) -> CapsInfo {
        let _g = self.caps_lock.lock().await;
        let info = caps::detect(core, self).await;
        let changed = {
            let mut cur = self.caps.lock().unwrap();
            let changed = cur.capabilities != info.capabilities;
            *cur = info.clone();
            changed
        };
        self.runner.set_capabilities(info.capabilities.clone());
        if changed {
            caps::announce(core, &info.capabilities);
            self.kick();
        }
        info
    }

    /// Remote Ollama URL in use (it answered the last capability probe), if any.
    pub fn remote_ollama_live(&self) -> Option<String> {
        self.remote_live.lock().unwrap().clone()
    }

    pub(crate) fn set_remote_live(&self, url: Option<String>) {
        let mut cur = self.remote_live.lock().unwrap();
        if *cur != url {
            match &url {
                Some(u) => tracing::info!("LLM: using the remote Ollama at {u}"),
                None => tracing::info!("LLM: using the local Ollama"),
            }
            *cur = url;
        }
    }

    fn spawn(&self, f: impl std::future::Future<Output = ()> + Send + 'static) {
        self.tasks.lock().unwrap().push(tokio::spawn(f));
    }
}

/// The Ollama LLM calls go to: the remote one (setting `llm.remote_ollama_url`) while it answers,
/// else `core.ollama_url()` made connectable (`OLLAMA_HOST=0.0.0.0:11434`, a server bind address,
/// becomes `http://127.0.0.1:11434`).
pub(crate) fn ollama_url(core: &Core) -> String {
    core.processing
        .remote_ollama_live()
        .unwrap_or_else(|| local_ollama_url(core))
}

pub(crate) fn local_ollama_url(core: &Core) -> String {
    almond_llm::ollama::normalize_host(&core.ollama_url())
}

/// Setting `llm.remote_ollama_url`, normalized (`100.80.179.59` -> `http://100.80.179.59:11434`).
pub(crate) fn remote_ollama_setting(core: &Core) -> Option<String> {
    core.setting_as::<String>(crate::settings::keys::REMOTE_OLLAMA_URL)
        .ok()
        .flatten()
        .and_then(|s| normalize_remote(&s))
}

/// `host`, `host:port` or a URL -> a URL with a port (Ollama's 11434 when none is given).
fn normalize_remote(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return None;
    }
    let (scheme, rest) = raw.split_once("://").unwrap_or(("http", raw));
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let has_port = authority.rsplit_once(':').is_some_and(|(h, p)| {
        !p.is_empty()
            && p.bytes().all(|b| b.is_ascii_digit())
            && !h.is_empty()
            && (!h.contains(':') || h.ends_with(']'))
    });
    let authority = if has_port {
        authority.to_string()
    } else {
        format!("{authority}:11434")
    };
    let path = if path.is_empty() {
        String::new()
    } else {
        format!("/{path}")
    };
    Some(format!("{scheme}://{authority}{path}"))
}

pub(crate) fn register(r: &mut Router) {
    api::register(r);
}

pub(crate) fn start(core: Arc<Core>) {
    let _ = core.processing.core.0.set(Arc::downgrade(&core));
    core.processing
        .runner
        .set_defer(Some(offload::defer_fn(Arc::downgrade(&core))));
    jobloop::start(&core);
}

#[cfg(test)]
mod tests {
    use super::normalize_remote;

    #[test]
    fn remote_url_normalization() {
        let n = |s: &str| normalize_remote(s);
        assert_eq!(
            n("100.80.179.59").as_deref(),
            Some("http://100.80.179.59:11434")
        );
        assert_eq!(
            n(" 100.80.179.59:8080/ ").as_deref(),
            Some("http://100.80.179.59:8080")
        );
        assert_eq!(
            n("http://pc.tail1234.ts.net").as_deref(),
            Some("http://pc.tail1234.ts.net:11434")
        );
        assert_eq!(n("https://pc:443").as_deref(), Some("https://pc:443"));
        assert_eq!(n("[fd7a::1]").as_deref(), Some("http://[fd7a::1]:11434"));
        assert_eq!(
            n("http://[fd7a::1]:9000").as_deref(),
            Some("http://[fd7a::1]:9000")
        );
        assert_eq!(n("  "), None);
    }
}
