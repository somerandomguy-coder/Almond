//! Capability detection and model slots.
//!
//! Capabilities (matched against `Job::requires` by the runner):
//! - static ones from `CoreConfig::capabilities`;
//! - `gpu` when `nvidia-smi` reports an NVIDIA GPU;
//! - `llm` + `llm:<model>` for every Ollama chat model, `embed:<model>` for embedding models;
//! - `stt:<lang>` / `diarize` for installed speech models (from `capture`).
//!
//! Model slots (setting `models.slots`, a `ModelSlot[]` of overrides): `llm_summary` (summary,
//! notes, action items, title) and `llm_verify` (fact-check judge). Defaults: `gemma4:12b` on a
//! GPU device when installed, else `qwen3.5:4b`, else the first installed chat model.
//!
//! Ollama models are identified as `ollama:<name>` in `models.list` and slots (the UI's
//! convention); a bare `<name>` is accepted too.

use super::{State, stubs};
use crate::Core;
use crate::settings::keys;
use almond_llm::ollama::canonical_model_name;
use almond_types::api::ModelSlot;
use almond_types::event::EventBody;
use almond_types::job::Capability;
use almond_types::llm::LlmModelInfo;
use almond_types::store::Store;
use almond_types::{Error, Result};
use serde::Serialize;
use std::sync::OnceLock;
use std::time::Duration;

pub const SLOT_LLM_SUMMARY: &str = "llm_summary";
pub const SLOT_LLM_VERIFY: &str = "llm_verify";
pub const SLOT_EMBED: &str = "embed";

/// Preferred models, best first, per device class (eval/summary/RESULTS.md, almond-verify EVAL.md).
const PREFERRED_GPU: &[&str] = &["gemma4:12b", "qwen3.5:4b"];
const PREFERRED_CPU: &[&str] = &["qwen3.5:4b", "gemma4:12b"];
const PREFERRED_EMBED: &[&str] = &["qwen3-embedding:0.6b"];

#[derive(Debug, Clone, Default, Serialize)]
pub struct CapsInfo {
    pub capabilities: Vec<Capability>,
    /// NVIDIA GPU name, if any.
    pub gpu: Option<String>,
    /// Models installed in Ollama (empty if unreachable).
    pub llm_models: Vec<LlmModelInfo>,
    /// Why no LLM is available (Ollama unreachable, ...).
    pub llm_error: Option<String>,
}

impl CapsInfo {
    pub fn has_model(&self, name: &str) -> bool {
        let want = canonical_model_name(ollama_name(name));
        self.llm_models
            .iter()
            .any(|m| canonical_model_name(&m.name) == want)
    }

    pub fn chat_models(&self) -> impl Iterator<Item = &LlmModelInfo> {
        self.llm_models.iter().filter(|m| !is_embed_model(&m.name))
    }
}

/// `ollama:gemma4:12b` -> `gemma4:12b`.
pub fn ollama_name(model: &str) -> &str {
    model.strip_prefix("ollama:").unwrap_or(model)
}

/// `gemma4:12b` -> `ollama:gemma4:12b` (the id in `models.list` and slots).
pub fn ollama_id(name: &str) -> String {
    format!("ollama:{}", ollama_name(name))
}

pub(crate) fn is_embed_model(name: &str) -> bool {
    name.to_ascii_lowercase().contains("embed")
}

/// `nvidia-smi` GPU name (cached for the process; GPUs don't come and go).
async fn detect_gpu(platform: &str) -> Option<String> {
    static GPU: OnceLock<Option<String>> = OnceLock::new();
    if platform == "android" {
        return None;
    }
    if let Some(g) = GPU.get() {
        return g.clone();
    }
    let g = tokio::task::spawn_blocking(|| {
        let mut cmd = std::process::Command::new("nvidia-smi");
        cmd.args(["--query-gpu=name", "--format=csv,noheader"]);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }
        let out = cmd.output().ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(String::from)
    })
    .await
    .ok()
    .flatten();
    GPU.get_or_init(|| g).clone()
}

pub(crate) async fn detect(core: &Core, st: &State) -> CapsInfo {
    let mut info = CapsInfo {
        capabilities: core.cfg.capabilities.clone(),
        ..Default::default()
    };
    info.gpu = detect_gpu(&core.cfg.platform).await;
    if info.gpu.is_some() {
        info.capabilities.push(Capability::new("gpu"));
    }
    match st.llm_provider(core) {
        Ok(p) => match tokio::time::timeout(Duration::from_secs(4), p.list_models()).await {
            Ok(Ok(models)) => info.llm_models = models,
            Ok(Err(e)) => info.llm_error = Some(e.to_string()),
            Err(_) => info.llm_error = Some(format!("{} did not answer", core.ollama_url())),
        },
        Err(e) => info.llm_error = Some(e.to_string()),
    }
    let mut any_chat = false;
    for m in &info.llm_models {
        if is_embed_model(&m.name) {
            info.capabilities
                .push(Capability::new(format!("embed:{}", m.name)));
        } else {
            any_chat = true;
            info.capabilities
                .push(Capability::new(format!("llm:{}", m.name)));
        }
    }
    if any_chat {
        info.capabilities.push(Capability::new("llm"));
    }
    match stubs::speech_models(core) {
        Ok(models) => {
            for m in models.into_iter().filter(|m| m.installed) {
                match m.kind.as_str() {
                    "stt" => {
                        for l in &m.languages {
                            info.capabilities.push(Capability::new(format!("stt:{l}")));
                        }
                    }
                    "diarize" => info.capabilities.push(Capability::new("diarize")),
                    _ => {}
                }
            }
        }
        Err(e) => tracing::warn!("speech models: {e}"),
    }
    info.capabilities.sort();
    info.capabilities.dedup();
    info
}

/// Record this device's detected capabilities in the replicated device list (if they changed).
pub(crate) fn announce(core: &Core, caps: &[Capability]) {
    let me = core.device_id().clone();
    let known = core
        .store()
        .devices()
        .ok()
        .and_then(|ds| ds.into_iter().find(|d| d.id == me));
    let mut info = known.unwrap_or_else(|| core.device().clone());
    if info.capabilities == caps {
        return;
    }
    info.capabilities = caps.to_vec();
    if let Err(e) = core.store().append_local(EventBody::DeviceInfo(info)) {
        tracing::warn!("announcing capabilities: {e}");
    }
}

fn pick(info: &CapsInfo, preferred: &[&str], embed: bool) -> Option<String> {
    preferred
        .iter()
        .find(|p| info.has_model(p))
        .map(|p| p.to_string())
        .or_else(|| {
            info.llm_models
                .iter()
                .find(|m| is_embed_model(&m.name) == embed)
                .map(|m| m.name.clone())
        })
}

/// Default slots for this device.
pub fn default_slots(info: &CapsInfo) -> Vec<ModelSlot> {
    let preferred = if info.gpu.is_some() {
        PREFERRED_GPU
    } else {
        PREFERRED_CPU
    };
    // Without any installed model keep the preferred name, so jobs fail with a clear message.
    let llm = ollama_id(&pick(info, preferred, false).unwrap_or_else(|| preferred[0].to_string()));
    let mut v = vec![
        ModelSlot {
            task: SLOT_LLM_SUMMARY.into(),
            lang: None,
            model: llm.clone(),
        },
        ModelSlot {
            task: SLOT_LLM_VERIFY.into(),
            lang: None,
            model: llm,
        },
    ];
    if let Some(e) = pick(info, PREFERRED_EMBED, true) {
        v.push(ModelSlot {
            task: SLOT_EMBED.into(),
            lang: None,
            model: ollama_id(&e),
        });
    }
    v
}

/// User overrides (setting `models.slots`).
pub(crate) fn stored_slots(core: &Core) -> Vec<ModelSlot> {
    core.setting_as::<Vec<ModelSlot>>(keys::MODEL_SLOTS)
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Overrides first, then defaults for every (task, lang) without one.
pub(crate) fn effective_slots(core: &Core, info: &CapsInfo) -> Vec<ModelSlot> {
    let mut v = stored_slots(core);
    for d in default_slots(info) {
        if !v.iter().any(|s| s.task == d.task && s.lang == d.lang) {
            v.push(d);
        }
    }
    v
}

/// Ollama model name for `task` and output language `lang`: a slot for that language, else the
/// all-languages slot.
pub(crate) fn slot_model(core: &Core, info: &CapsInfo, task: &str, lang: Option<&str>) -> String {
    let slots = effective_slots(core, info);
    let for_task = |l: Option<&str>| {
        slots
            .iter()
            .find(|s| s.task == task && s.lang.as_deref() == l)
            .map(|s| s.model.clone())
    };
    let m = lang
        .and_then(|l| for_task(Some(l)))
        .or_else(|| for_task(None))
        .unwrap_or_else(|| default_slots(info)[0].model.clone());
    ollama_name(&m).to_string()
}

/// `models.select`: store an override (replacing the one for the same task + language).
/// LLM slots must name an installed Ollama model.
pub(crate) fn select(core: &Core, info: &CapsInfo, slot: ModelSlot) -> Result<()> {
    if slot.model.trim().is_empty() {
        return Err(Error::Invalid("model must not be empty".into()));
    }
    if slot.task.starts_with("llm") || slot.task == SLOT_EMBED {
        if let Some(e) = &info.llm_error
            && info.llm_models.is_empty()
        {
            return Err(Error::Invalid(format!(
                "cannot select {}: Ollama is not available at {} ({e})",
                slot.model,
                core.ollama_url()
            )));
        }
        if !info.has_model(&slot.model) {
            let name = ollama_name(&slot.model);
            return Err(Error::NotFound(format!(
                "model {name} is not installed in Ollama at {} (run `ollama pull {name}`)",
                core.ollama_url(),
            )));
        }
    }
    let mut v = stored_slots(core);
    v.retain(|s| !(s.task == slot.task && s.lang == slot.lang));
    v.push(slot);
    core.set_setting(keys::MODEL_SLOTS, &serde_json::to_value(v)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(gpu: bool, models: &[&str]) -> CapsInfo {
        CapsInfo {
            gpu: gpu.then(|| "RTX".to_string()),
            llm_models: models
                .iter()
                .map(|m| LlmModelInfo {
                    name: m.to_string(),
                    digest: None,
                    size_bytes: None,
                    context_length: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn default_slot_choice() {
        let all = ["qwen3-embedding:0.6b", "gemma4:12b", "qwen3.5:4b"];
        let s = default_slots(&info(true, &all));
        assert_eq!(s[0].model, "ollama:gemma4:12b");
        assert_eq!(s[1].model, "ollama:gemma4:12b");
        assert_eq!(s[2].model, "ollama:qwen3-embedding:0.6b");
        assert_eq!(
            default_slots(&info(false, &all))[0].model,
            "ollama:qwen3.5:4b"
        );
        assert_eq!(
            default_slots(&info(true, &["llama3.1:8b"]))[0].model,
            "ollama:llama3.1:8b"
        );
        // Nothing installed: keep the preferred name (jobs then fail loudly).
        let none = default_slots(&info(false, &[]));
        assert_eq!(none[0].model, "ollama:qwen3.5:4b");
        assert!(info(true, &all).has_model("ollama:gemma4:12b"));
        assert_eq!(none.len(), 2);
        assert!(info(false, &["qwen3.5"]).has_model("qwen3.5:latest"));
    }
}
