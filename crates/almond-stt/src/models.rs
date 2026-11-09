//! Model registry + manager.
//!
//! * The **registry** (`models.json`, compiled in; can be extended by
//!   `<models_dir>/registry.local.json`) lists every model we know: id, engine, languages,
//!   download source (url, sha256, size, archive type) and the files inside.
//! * The **manager** resolves the models directory (`ALMOND_MODELS_DIR` or
//!   `~/.almond/models`, shared between worktrees/apps, never inside the repo), reports what is
//!   installed, and downloads with HTTP range **resume** + **SHA-256** verification
//!   (feature `download`).
//!
//! PhoWhisper is not distributed in ggml format by VinAI; its entries have `source.kind =
//! "convert"` and are produced locally by `eval/stt/convert_phowhisper.py` (see that script).

use almond_types::stt::SttModelInfo;
use almond_types::{Error, LangTag, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

const BUILTIN_REGISTRY: &str = include_str!("../models.json");

/// What a model is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// Speech recognition.
    Stt,
    /// Speaker segmentation (pyannote).
    Segmentation,
    /// Speaker embedding extractor.
    Embedding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Single file downloaded as-is.
    File,
    /// `.tar.bz2` archive, extracted into the model dir (top-level folder stripped).
    TarBz2,
    /// Produced locally by a conversion script (`convert` field says how).
    Convert,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelSource {
    pub kind: SourceKind,
    pub url: Option<String>,
    /// Lowercase hex SHA-256 of the downloaded file (archive for `tar_bz2`). `None` = not yet
    /// pinned; the manager then logs the computed hash so it can be added to the registry.
    pub sha256: Option<String>,
    pub size: Option<u64>,
    /// For `convert`: human-readable instructions / script.
    pub convert: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelEntry {
    /// "whisper:large-v3-turbo-q5_0", "sherpa:zipformer-vi-30m-int8", ...
    pub id: String,
    /// "whisper" | "sherpa-transducer" | "sherpa-pyannote" | "sherpa-embedding"
    pub engine: String,
    pub role: ModelRole,
    pub languages: Vec<LangTag>,
    /// Natively streaming (we still wrap offline models in pseudo-streaming).
    pub streaming: bool,
    pub source: ModelSource,
    /// Logical name -> path relative to the model dir, e.g. {"model": "ggml-....bin"} or
    /// {"encoder": "...", "decoder": "...", "joiner": "...", "tokens": "tokens.txt"}.
    pub files: BTreeMap<String, String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

impl ModelEntry {
    /// Directory name for this model ("whisper:small" -> "whisper__small").
    pub fn dir_name(&self) -> String {
        self.id.replace([':', '/', '\\'], "__")
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryFile {
    models: Vec<ModelEntry>,
}

/// The set of known models.
#[derive(Debug, Clone)]
pub struct Registry {
    pub models: Vec<ModelEntry>,
}

impl Registry {
    /// Built-in registry only.
    pub fn builtin() -> Self {
        let f: RegistryFile =
            serde_json::from_str(BUILTIN_REGISTRY).expect("models.json is valid (tested)");
        Registry { models: f.models }
    }

    /// Built-in registry plus `<dir>/registry.local.json` (entries with the same id override).
    pub fn load(models_dir: &Path) -> Result<Self> {
        let mut reg = Self::builtin();
        let local = models_dir.join("registry.local.json");
        if local.exists() {
            let f: RegistryFile = serde_json::from_slice(&std::fs::read(&local)?)?;
            for m in f.models {
                reg.models.retain(|x| x.id != m.id);
                reg.models.push(m);
            }
        }
        Ok(reg)
    }

    pub fn get(&self, id: &str) -> Option<&ModelEntry> {
        self.models.iter().find(|m| m.id == id)
    }
}

/// Default models directory: `$ALMOND_MODELS_DIR`, else `~/.almond/models`.
pub fn default_models_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ALMOND_MODELS_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".almond").join("models")
}

/// Download progress callback: (bytes_done, total_bytes_if_known).
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// Resolves, verifies and installs models.
#[derive(Debug, Clone)]
pub struct ModelManager {
    pub dir: PathBuf,
    pub registry: Registry,
}

impl ModelManager {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        let registry = Registry::load(&dir)?;
        Ok(ModelManager { dir, registry })
    }

    pub fn open_default() -> Result<Self> {
        Self::new(default_models_dir())
    }

    pub fn entry(&self, id: &str) -> Result<&ModelEntry> {
        self.registry
            .get(id)
            .ok_or_else(|| Error::NotFound(format!("model {id} not in registry")))
    }

    pub fn model_dir(&self, id: &str) -> Result<PathBuf> {
        Ok(self.dir.join(self.entry(id)?.dir_name()))
    }

    /// Absolute path of a model's logical file ("model", "encoder", "tokens", ...).
    pub fn file(&self, id: &str, logical: &str) -> Result<PathBuf> {
        let e = self.entry(id)?;
        let rel = e
            .files
            .get(logical)
            .ok_or_else(|| Error::NotFound(format!("model {id} has no file {logical:?}")))?;
        Ok(self.dir.join(e.dir_name()).join(rel))
    }

    /// All declared files exist.
    pub fn is_installed(&self, id: &str) -> bool {
        let Ok(e) = self.entry(id) else { return false };
        let d = self.dir.join(e.dir_name());
        !e.files.is_empty() && e.files.values().all(|f| d.join(f).is_file())
    }

    /// `SttModelInfo` (contract type) for a registry entry.
    pub fn info(&self, id: &str) -> Result<SttModelInfo> {
        let e = self.entry(id)?;
        Ok(SttModelInfo {
            id: e.id.clone(),
            engine: e.engine.clone(),
            languages: e.languages.clone(),
            streaming: e.streaming,
            size_bytes: e.source.size,
            sha256: e.source.sha256.clone(),
            installed: self.is_installed(id),
        })
    }

    pub fn list(&self) -> Vec<SttModelInfo> {
        self.registry
            .models
            .iter()
            .filter_map(|m| self.info(&m.id).ok())
            .collect()
    }

    /// Download + verify + extract a model if not installed. Returns the model dir.
    #[cfg(feature = "download")]
    pub fn ensure(&self, id: &str, progress: Progress<'_>) -> Result<PathBuf> {
        let e = self.entry(id)?.clone();
        let dir = self.dir.join(e.dir_name());
        if self.is_installed(id) {
            return Ok(dir);
        }
        let url = match (&e.source.kind, &e.source.url) {
            (SourceKind::Convert, _) => {
                return Err(Error::Unsupported(format!(
                    "model {id} must be converted locally: {}",
                    e.source.convert.as_deref().unwrap_or("see registry notes")
                )));
            }
            (_, Some(u)) => u.clone(),
            (_, None) => return Err(Error::Invalid(format!("model {id} has no url"))),
        };
        std::fs::create_dir_all(&dir)?;
        let fname = url.rsplit('/').next().unwrap_or("download").to_string();
        let dl_path = match e.source.kind {
            SourceKind::File => dir.join(e.files.values().next().cloned().unwrap_or(fname)),
            _ => self.dir.join(".downloads").join(&fname),
        };
        if let Some(p) = dl_path.parent() {
            std::fs::create_dir_all(p)?;
        }
        download_resumable(&url, &dl_path, e.source.size, progress)?;
        let got = sha256_file(&dl_path)?;
        match &e.source.sha256 {
            Some(want) if !want.eq_ignore_ascii_case(&got) => {
                // keep nothing that failed verification
                std::fs::remove_file(&dl_path).ok();
                return Err(Error::Invalid(format!(
                    "sha256 mismatch for {id}: expected {want}, got {got}"
                )));
            }
            Some(_) => {}
            None => tracing::warn!(model = id, sha256 = %got, "model has no pinned sha256"),
        }
        if e.source.kind == SourceKind::TarBz2 {
            extract_tar_bz2_strip1(&dl_path, &dir)?;
            std::fs::remove_file(&dl_path).ok();
        }
        if !self.is_installed(id) {
            return Err(Error::Invalid(format!(
                "model {id} downloaded but expected files are missing in {}",
                dir.display()
            )));
        }
        Ok(dir)
    }
}

/// Lowercase hex SHA-256 of a file.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// HTTP GET with resume: data goes to `<dest>.part`, continuing from its current length via a
/// `Range` header; renamed to `dest` when complete.
#[cfg(feature = "download")]
pub fn download_resumable(
    url: &str,
    dest: &Path,
    expected_size: Option<u64>,
    progress: Progress<'_>,
) -> Result<()> {
    use std::io::Write;
    if dest.is_file()
        && expected_size.is_none_or(|s| dest.metadata().map(|m| m.len()).ok() == Some(s))
    {
        return Ok(());
    }
    let part = dest.with_extension(
        dest.extension()
            .map(|e| format!("{}.part", e.to_string_lossy()))
            .unwrap_or_else(|| "part".into()),
    );
    let have = part.metadata().map(|m| m.len()).unwrap_or(0);
    if expected_size == Some(have) && have > 0 {
        std::fs::rename(&part, dest)?;
        return Ok(());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .into();
    let mut req = agent.get(url);
    if have > 0 {
        req = req.header("Range", &format!("bytes={have}-"));
    }
    let mut resp = req
        .call()
        .map_err(|e| Error::Network(format!("GET {url}: {e}")))?;
    let status = resp.status().as_u16();
    let (mut file, mut done) = match status {
        206 => (std::fs::OpenOptions::new().append(true).open(&part)?, have),
        200 => (std::fs::File::create(&part)?, 0),
        416 if have > 0 => {
            // Already complete on disk.
            std::fs::rename(&part, dest)?;
            return Ok(());
        }
        s => return Err(Error::Network(format!("GET {url}: HTTP {s}"))),
    };
    let total = expected_size.or_else(|| {
        resp.headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|l| l + done)
    });
    let mut reader = resp.body_mut().as_reader();
    let mut buf = vec![0u8; 1 << 20];
    let mut last_report = 0u64;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| Error::Network(format!("reading {url}: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        if done - last_report >= 4 << 20 {
            progress(done, total);
            last_report = done;
        }
    }
    file.flush()?;
    drop(file);
    progress(done, total);
    if let Some(t) = total
        && done != t
    {
        return Err(Error::Network(format!(
            "incomplete download of {url}: {done}/{t} bytes (rerun to resume)"
        )));
    }
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// Extract a `.tar.bz2`, stripping the top-level directory.
#[cfg(feature = "download")]
pub fn extract_tar_bz2_strip1(archive: &Path, into: &Path) -> Result<()> {
    let f = std::fs::File::open(archive)?;
    let mut ar = tar::Archive::new(bzip2::read::BzDecoder::new(f));
    for entry in ar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let stripped: PathBuf = path.components().skip(1).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }
        // reject path traversal
        if stripped
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(Error::Invalid(format!(
                "unsafe path in archive: {}",
                path.display()
            )));
        }
        let out = into.join(&stripped);
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p)?;
        }
        if entry.header().entry_type().is_file() {
            entry.unpack(&out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_registry_parses_and_ids_unique() {
        let r = Registry::builtin();
        assert!(r.models.len() >= 8);
        let mut ids: Vec<_> = r.models.iter().map(|m| m.id.as_str()).collect();
        ids.sort();
        let n = ids.len();
        ids.dedup();
        assert_eq!(n, ids.len(), "duplicate ids");
        for m in &r.models {
            assert!(!m.files.is_empty(), "{} has no files", m.id);
            if let Some(h) = &m.source.sha256 {
                assert_eq!(h.len(), 64, "{} sha256", m.id);
            }
            if m.source.kind != SourceKind::Convert {
                assert!(m.source.url.is_some(), "{} url", m.id);
            }
        }
    }

    #[test]
    fn local_registry_overrides() {
        let dir = std::env::temp_dir().join(format!("almond-models-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("registry.local.json"),
            r#"{"models":[{"id":"whisper:custom","engine":"whisper","role":"stt",
               "languages":["vi"],"streaming":false,
               "source":{"kind":"file","url":"http://x/y.bin","sha256":null,"size":null,"convert":null},
               "files":{"model":"y.bin"}}]}"#,
        )
        .unwrap();
        let m = ModelManager::new(&dir).unwrap();
        assert!(m.registry.get("whisper:custom").is_some());
        assert!(!m.is_installed("whisper:custom"));
        std::fs::write(dir.join("whisper__custom").join("y.bin"), b"x")
            .or_else(|_| {
                std::fs::create_dir_all(dir.join("whisper__custom"))?;
                std::fs::write(dir.join("whisper__custom").join("y.bin"), b"x")
            })
            .unwrap();
        assert!(m.is_installed("whisper:custom"));
        assert!(m.info("whisper:custom").unwrap().installed);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn sha256_known_value() {
        let p = std::env::temp_dir().join(format!("almond-sha-{}", std::process::id()));
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha256_file(&p).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&p).ok();
    }

    /// Network test: downloads a ~26 MB archive, interrupts nothing but exercises the full path.
    /// Run: `cargo test -p almond-stt -- --ignored download_small_model`
    #[cfg(feature = "download")]
    #[test]
    #[ignore = "network"]
    fn download_small_model() {
        let m = ModelManager::open_default().unwrap();
        m.ensure("sherpa:pyannote-seg-3.0", &mut |_, _| {}).unwrap();
        assert!(m.is_installed("sherpa:pyannote-seg-3.0"));
    }
}
