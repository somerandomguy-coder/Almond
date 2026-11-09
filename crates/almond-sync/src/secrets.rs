//! `SecretStore` implementations.
//!
//! - [`KeyringSecretStore`] (Windows Credential Manager / macOS Keychain) via `keyring`.
//! - [`FileSecretStore`]: a JSON file with owner-only permissions (0600 on Unix). Used on Linux
//!   (no reliable always-on secret service on minimal desktops) and Android until WS-G provides
//!   a Keystore-backed implementation.
//! - [`FallbackSecretStore`]: primary with fallback on platform errors.
//!
//! [`default_secret_store`] picks the right one for the current platform.

use almond_types::secrets::SecretStore;
use almond_types::{Error, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Secrets in a JSON file (`{"key": "<hex>"}`), written atomically, 0600 on Unix.
pub struct FileSecretStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileSecretStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        FileSecretStore {
            path: path.into(),
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> Result<BTreeMap<String, String>> {
        match std::fs::read(&self.path) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, m: &BTreeMap<String, String>) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        write_private(&tmp, &serde_json::to_vec_pretty(m)?)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

#[cfg(unix)]
fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(p)?;
    f.write_all(data)?;
    f.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(p: &Path, data: &[u8]) -> Result<()> {
    // Windows: the file lives under the per-user app data dir, which is already user-only.
    std::fs::write(p, data)?;
    Ok(())
}

impl SecretStore for FileSecretStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.load()?
            .get(key)
            .map(|h| {
                data_encoding::HEXLOWER
                    .decode(h.as_bytes())
                    .map_err(|e| Error::Invalid(format!("secret {key}: {e}")))
            })
            .transpose()
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load()?;
        m.insert(key.to_string(), data_encoding::HEXLOWER.encode(value));
        self.save(&m)
    }

    fn delete(&self, key: &str) -> Result<()> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load()?;
        if m.remove(key).is_some() {
            self.save(&m)?;
        }
        Ok(())
    }
}

/// OS keychain (Windows Credential Manager, macOS Keychain). Service name `almond`.
#[cfg(any(windows, target_os = "macos"))]
pub struct KeyringSecretStore {
    service: String,
}

#[cfg(any(windows, target_os = "macos"))]
impl KeyringSecretStore {
    pub fn new(service: impl Into<String>) -> Self {
        KeyringSecretStore {
            service: service.into(),
        }
    }

    fn entry(&self, key: &str) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, key).map_err(|e| Error::Other(format!("keyring: {e}")))
    }
}

#[cfg(any(windows, target_os = "macos"))]
impl SecretStore for KeyringSecretStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.entry(key)?.get_secret() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(Error::Other(format!("keyring: {e}"))),
        }
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        self.entry(key)?
            .set_secret(value)
            .map_err(|e| Error::Other(format!("keyring: {e}")))
    }
    fn delete(&self, key: &str) -> Result<()> {
        match self.entry(key)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Error::Other(format!("keyring: {e}"))),
        }
    }
}

/// Use `primary`; on a platform error (not "missing") fall back to `fallback`. Reads check the
/// fallback too, so a secret written during an outage of the primary is still found.
pub struct FallbackSecretStore {
    primary: Arc<dyn SecretStore>,
    fallback: Arc<dyn SecretStore>,
}

impl FallbackSecretStore {
    pub fn new(primary: Arc<dyn SecretStore>, fallback: Arc<dyn SecretStore>) -> Self {
        FallbackSecretStore { primary, fallback }
    }
}

impl SecretStore for FallbackSecretStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        match self.primary.get(key) {
            Ok(Some(v)) => Ok(Some(v)),
            Ok(None) | Err(_) => self.fallback.get(key),
        }
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        match self.primary.set(key, value) {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::warn!("primary secret store failed ({e}); using fallback file");
                self.fallback.set(key, value)
            }
        }
    }
    fn delete(&self, key: &str) -> Result<()> {
        let a = self.primary.delete(key);
        let b = self.fallback.delete(key);
        a.and(b)
    }
}

/// Platform default: OS keychain with file fallback on Windows/macOS; file elsewhere.
pub fn default_secret_store(data_dir: &Path) -> Arc<dyn SecretStore> {
    let file: Arc<dyn SecretStore> = Arc::new(FileSecretStore::new(data_dir.join("secrets.json")));
    #[cfg(any(windows, target_os = "macos"))]
    {
        Arc::new(FallbackSecretStore::new(
            Arc::new(KeyringSecretStore::new("almond")),
            file,
        ))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        file
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let s = FileSecretStore::new(dir.path().join("s/secrets.json"));
        assert_eq!(s.get("k").unwrap(), None);
        s.set("k", &[1, 2, 255]).unwrap();
        s.set("j", b"x").unwrap();
        let s2 = FileSecretStore::new(dir.path().join("s/secrets.json"));
        assert_eq!(s2.get("k").unwrap(), Some(vec![1, 2, 255]));
        s2.delete("k").unwrap();
        assert_eq!(s.get("k").unwrap(), None);
        assert_eq!(s.get("j").unwrap(), Some(b"x".to_vec()));
    }

    /// Touches the real OS keychain. Run with `cargo test -p almond-sync -- --ignored keyring`.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    #[ignore]
    fn keyring_roundtrip() {
        let s = KeyringSecretStore::new("almond-test");
        s.set("almond/test", b"secret").unwrap();
        assert_eq!(s.get("almond/test").unwrap(), Some(b"secret".to_vec()));
        s.delete("almond/test").unwrap();
        assert_eq!(s.get("almond/test").unwrap(), None);
    }
}
