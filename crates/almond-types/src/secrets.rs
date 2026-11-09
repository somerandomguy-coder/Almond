//! Secret storage (OS keychain on desktop, Android Keystore on phone). Never synced.

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> crate::Result<Option<Vec<u8>>>;
    fn set(&self, key: &str, value: &[u8]) -> crate::Result<()>;
    fn delete(&self, key: &str) -> crate::Result<()>;
}

/// Well-known secret keys.
pub mod keys {
    /// ed25519 secret key of this device.
    pub const DEVICE_KEY: &str = "almond/device_key";
    /// `format!("{GOOGLE_REFRESH_PREFIX}{email}")`
    pub const GOOGLE_REFRESH_PREFIX: &str = "almond/google/refresh/";
    /// Local API bearer token.
    pub const API_TOKEN: &str = "almond/api_token";
}
