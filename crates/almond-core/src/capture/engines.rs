//! Speech engines for live captions and `transcribe.local`, picked by
//! `almond_stt::routing::RoutingTable::default_for(device class)` and cached once loaded.
//!
//! Device class: built with `cuda` = desktop with GPU (Zipformer-30M for Vietnamese,
//! `whisper:large-v3-turbo` for English and language ID); Android = mobile; otherwise desktop CPU.
//! Without the `whisper`/`sherpa` features no engine can load and callers get `Unsupported`.

use almond_stt::live::{LiveModel, LiveRouter};
use almond_stt::models::ModelManager;
use almond_stt::router::{LanguageId, RoutedEngine};
use almond_stt::routing::{DeviceClass, RoutingTable};
use almond_types::stt::{Diarizer, SttEngine, SttOptions};
use almond_types::{Error, LangTag, Result};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

/// Engines to use instead of the native ones (tests with `almond_types::mock::MockStt`).
#[derive(Clone)]
pub struct SpeechOverride {
    pub offline: Arc<dyn SttEngine>,
    pub live: Arc<dyn SttEngine>,
    pub diarizer: Option<Arc<dyn Diarizer>>,
}

#[derive(Clone)]
struct Loaded {
    engine: Arc<dyn SttEngine>,
    lid: Option<Arc<dyn LanguageId>>,
    live: Option<Arc<dyn LiveModel>>,
}

/// What [`Speech::offline`] chose.
pub struct OfflineSetup {
    pub engine: Arc<dyn SttEngine>,
    /// Options adjusted to what loaded (e.g. a fixed language when no language-ID model is
    /// available).
    pub opts: SttOptions,
    /// Model ids, for provenance ("vi=sherpa:zipformer-vi-30m-int8,en=whisper:large-v3-turbo").
    pub models: String,
}

pub struct Speech {
    models_dir: PathBuf,
    device: DeviceClass,
    cache: Mutex<BTreeMap<String, Loaded>>,
    load_lock: Mutex<()>,
    diarizer: Mutex<Option<Arc<dyn Diarizer>>>,
    over: RwLock<Option<SpeechOverride>>,
}

pub fn device_class() -> DeviceClass {
    if cfg!(target_os = "android") {
        DeviceClass::Mobile
    } else if cfg!(feature = "cuda") {
        DeviceClass::DesktopGpu
    } else {
        DeviceClass::DesktopCpu
    }
}

/// Default language for live captions and when nothing else is known.
pub fn default_language() -> LangTag {
    LangTag::vi()
}

impl Speech {
    pub fn new(models_dir: PathBuf) -> Self {
        Speech {
            models_dir,
            device: device_class(),
            cache: Default::default(),
            load_lock: Default::default(),
            diarizer: Default::default(),
            over: Default::default(),
        }
    }

    pub fn set_override(&self, o: Option<SpeechOverride>) {
        *self.over.write().unwrap_or_else(|p| p.into_inner()) = o;
    }

    fn override_(&self) -> Option<SpeechOverride> {
        self.over.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn table(&self) -> RoutingTable {
        RoutingTable::default_for(self.device)
    }

    pub fn manager(&self) -> Result<ModelManager> {
        ModelManager::new(&self.models_dir)
    }

    /// Load (or get from cache) one registry model. Blocking.
    fn load(&self, id: &str) -> Result<Loaded> {
        if let Some(l) = self.cache.lock().unwrap_or_else(|p| p.into_inner()).get(id) {
            return Ok(l.clone());
        }
        // One load at a time (two caption streams start together); re-check under the lock.
        let _guard = self.load_lock.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(l) = self.cache.lock().unwrap_or_else(|p| p.into_inner()).get(id) {
            return Ok(l.clone());
        }
        let mgr = self.manager()?;
        let entry = mgr.entry(id)?;
        // Say "not compiled in" before "not installed": downloading won't help a build without
        // the engine (and this makes the error independent of which models a machine has).
        let compiled = match entry.engine.as_str() {
            "whisper" => cfg!(feature = "whisper"),
            "sherpa-transducer" => cfg!(feature = "sherpa"),
            _ => false,
        };
        if !compiled {
            return Err(Error::Unsupported(format!(
                "speech engine {:?} (model {id}) is not compiled into this build; \
                 build with --features whisper,sherpa (cuda on the GPU PC)",
                entry.engine
            )));
        }
        if !mgr.is_installed(id) {
            return Err(Error::NotFound(format!(
                "speech model {id} is not installed (download it under Settings → Models)"
            )));
        }
        let loaded: Result<Loaded> = match entry.engine.as_str() {
            #[cfg(feature = "whisper")]
            "whisper" => {
                let m = Arc::new(almond_stt::whisper::WhisperModel::from_manager(
                    &mgr,
                    id,
                    Default::default(),
                )?);
                let e = Arc::new(almond_stt::whisper::WhisperEngine::new(m.clone()));
                Ok(Loaded {
                    engine: e.clone(),
                    lid: Some(m),
                    live: Some(e),
                })
            }
            #[cfg(feature = "sherpa")]
            "sherpa-transducer" => {
                let m = Arc::new(almond_stt::sherpa::ZipformerModel::from_manager(
                    &mgr,
                    id,
                    Default::default(),
                )?);
                let e = Arc::new(almond_stt::sherpa::SherpaEngine::new(m));
                Ok(Loaded {
                    engine: e.clone(),
                    lid: None,
                    live: Some(e),
                })
            }
            other => Err(Error::Unsupported(format!(
                "speech engine {other:?} (model {id}) is not compiled into this build; \
                 build with --features whisper,sherpa (cuda on the GPU PC)"
            ))),
        };
        let loaded = loaded?;
        tracing::info!(model = id, "speech model loaded");
        self.cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.to_string(), loaded.clone());
        Ok(loaded)
    }

    /// Drop loaded models (frees GPU memory for Ollama between meetings).
    pub fn unload_all(&self) {
        self.cache.lock().unwrap_or_else(|p| p.into_inner()).clear();
        *self.diarizer.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// Engine for live captions in `languages` (primary first). Blocking (loads models on first
    /// use).
    ///
    /// With several languages and a language-ID model this is an
    /// [`almond_stt::live::LiveRouter`]: partials from the live model of the previous final's
    /// language, every final re-decoded by the offline model of each part's language (language
    /// decided per pause-separated part of the segment). Otherwise the primary language's live
    /// model, or any live-capable model if that one is missing.
    pub fn live(&self, languages: &[LangTag]) -> Result<Arc<dyn SttEngine>> {
        if let Some(o) = self.override_() {
            return Ok(o.live);
        }
        let primary = languages.first().cloned().unwrap_or_else(default_language);
        let t = self.table();
        let lid = self
            .load(&t.fallback.offline)
            .ok()
            .and_then(|l| l.lid.clone());
        let mut partial: BTreeMap<String, Arc<dyn LiveModel>> = BTreeMap::new();
        let mut finals: BTreeMap<String, Arc<dyn LiveModel>> = BTreeMap::new();
        let mut langs = vec![];
        for l in languages {
            let r = t.route(Some(l));
            let Some(p) = self.load(&r.live).ok().and_then(|x| x.live) else {
                tracing::warn!("live captions: {} for {} unavailable", r.live, l.0);
                continue;
            };
            partial.insert(l.primary(), p);
            if let Some(f) = self.load(&r.offline).ok().and_then(|x| x.live) {
                finals.insert(l.primary(), f);
            }
            langs.push(LangTag(l.primary()));
        }
        if langs.first().map(|l| l.primary()) == Some(primary.primary()) {
            return Ok(Arc::new(LiveRouter {
                lid: if langs.len() > 1 { lid } else { None },
                langs,
                partial,
                finals,
                policy: Default::default(),
                // Partials follow the language of the previous final; language ID per partial
                // decode made Vietnamese worse (17.9 % vs 13.4 % WER on the mixed test
                // conversation, eval/stt/RESULTS.md) and costs an encoder pass per step.
                lid_partials: false,
                stream: Default::default(),
            }));
        }
        // The primary language's live model is missing: any live-capable model is better
        // than no captions.
        let want = t.route(Some(&primary)).live.clone();
        let mut first_err = None;
        for id in std::iter::once(want.clone()).chain(t.model_ids()) {
            match self.load(&id) {
                Ok(l) => {
                    if id != want {
                        tracing::warn!("live captions: {want} unavailable; using {id}");
                    }
                    return Ok(l.engine);
                }
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        Err(first_err.unwrap_or_else(|| Error::NotFound("no live caption model".into())))
    }

    /// Routed offline engine for `languages` (language ID per utterance when several are
    /// allowed and a Whisper LID model is available). Blocking.
    pub fn offline(&self, languages: &[LangTag]) -> Result<OfflineSetup> {
        let mut opts = SttOptions {
            allowed_languages: languages.to_vec(),
            word_timestamps: true,
            ..Default::default()
        };
        if let Some(o) = self.override_() {
            return Ok(OfflineSetup {
                models: o.offline.model().id,
                engine: o.offline,
                opts,
            });
        }
        let t = self.table();
        let mut engines: BTreeMap<String, Arc<dyn SttEngine>> = BTreeMap::new();
        let mut names = vec![];
        let mut first_err = None;
        for l in languages {
            let id = &t.route(Some(l)).offline;
            match self.load(id) {
                Ok(x) => {
                    engines.insert(l.primary(), x.engine);
                    names.push(format!("{}={id}", l.primary()));
                }
                Err(e) => {
                    tracing::warn!("transcription: {id} for {} unavailable: {e}", l.0);
                    first_err.get_or_insert(e);
                }
            }
        }
        let lid_id = &t.fallback.offline;
        let fallback = self.load(lid_id);
        let lid = fallback.as_ref().ok().and_then(|f| f.lid.clone());
        let fallback_engine = match &fallback {
            Ok(f) => f.engine.clone(),
            Err(_) => match engines.values().next() {
                Some(e) => e.clone(),
                None => {
                    return Err(first_err.unwrap_or_else(|| {
                        Error::Invalid("no language given for transcription".into())
                    }));
                }
            },
        };
        if fallback.is_ok() && !names.iter().any(|n| n.ends_with(&format!("={lid_id}"))) {
            names.push(format!("other={lid_id}"));
        }
        let lid: Arc<dyn LanguageId> = match lid {
            Some(l) => l,
            None => {
                // No language ID: transcribe everything in the first language that loaded.
                let l = languages
                    .iter()
                    .find(|l| engines.contains_key(&l.primary()))
                    .cloned()
                    .unwrap_or_else(default_language);
                tracing::warn!(
                    "no language-ID model ({lid_id}); transcribing everything as {}",
                    l.0
                );
                opts.language = Some(l);
                Arc::new(NoLid)
            }
        };
        Ok(OfflineSetup {
            engine: Arc::new(RoutedEngine::new(lid, engines, fallback_engine)),
            opts,
            models: names.join(","),
        })
    }

    /// Diarizer for the system / mixed channel; `None` when its models or the `sherpa` feature
    /// are missing (the transcript then has no remote speaker labels). Blocking.
    pub fn diarizer(&self) -> Option<Arc<dyn Diarizer>> {
        if let Some(o) = self.override_() {
            return o.diarizer;
        }
        let mut d = self.diarizer.lock().unwrap_or_else(|p| p.into_inner());
        if d.is_none() {
            *d = self.load_diarizer();
        }
        d.clone()
    }

    #[cfg(feature = "sherpa")]
    fn load_diarizer(&self) -> Option<Arc<dyn Diarizer>> {
        let mgr = self.manager().ok()?;
        match almond_stt::sherpa::SherpaDiarizer::from_manager(&mgr, Default::default()) {
            Ok(d) => Some(Arc::new(d)),
            Err(e) => {
                tracing::warn!("diarization unavailable: {e}");
                None
            }
        }
    }

    #[cfg(not(feature = "sherpa"))]
    fn load_diarizer(&self) -> Option<Arc<dyn Diarizer>> {
        None
    }
}

/// Placeholder when no language-ID model exists; never called because the language is fixed.
struct NoLid;

impl LanguageId for NoLid {
    fn identify(&self, _: &[f32], allowed: &[LangTag]) -> Result<(LangTag, f32)> {
        Ok((
            allowed.first().cloned().unwrap_or_else(default_language),
            0.0,
        ))
    }
}
