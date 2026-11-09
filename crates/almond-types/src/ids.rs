use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use ts_rs::TS;

/// Globally unique, time-sortable id (ULID string, 26 chars). Can be minted on any device.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Id(pub String);

impl Id {
    pub fn new() -> Self {
        Id(ulid::Ulid::generate().to_string())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Id {
    type Err = crate::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ulid::Ulid::from_string(s)
            .map(|u| Id(u.to_string()))
            .map_err(|e| crate::Error::Invalid(format!("bad id {s:?}: {e}")))
    }
}

/// A device's public identity: the ed25519 public key as lowercase hex / base32 string
/// (identical to the iroh node id). Never contains secret material.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeviceId(pub String);

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// BLAKE3 hash (lowercase hex) of a blob's bytes. Blobs are content-addressed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct BlobHash(pub String);

/// Speaker label within one meeting ("S1", "me", or a resolved contact email).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SpeakerId(pub String);
