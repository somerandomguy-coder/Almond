//! Device identity: an ed25519 keypair whose public key *is* the iroh endpoint id and the
//! Almond `DeviceId` (lowercase hex). The secret half lives only in the `SecretStore`.

use almond_types::secrets::{SecretStore, keys};
use almond_types::{DeviceId, Error, Result};
use iroh::{EndpointId, SecretKey};
use std::str::FromStr;

#[derive(Clone)]
pub struct DeviceIdentity {
    secret: SecretKey,
}

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceIdentity({})", self.device_id())
    }
}

impl DeviceIdentity {
    /// Load the device key from the secret store, creating (and saving) one on first run.
    pub fn load_or_create(secrets: &dyn SecretStore) -> Result<Self> {
        if let Some(b) = secrets.get(keys::DEVICE_KEY)? {
            let arr: [u8; 32] = b
                .as_slice()
                .try_into()
                .map_err(|_| Error::Invalid("device key must be 32 bytes".into()))?;
            return Ok(DeviceIdentity {
                secret: SecretKey::from_bytes(&arr),
            });
        }
        let id = Self::generate();
        secrets.set(keys::DEVICE_KEY, &id.secret.to_bytes())?;
        Ok(id)
    }

    /// Ephemeral identity (tests).
    pub fn generate() -> Self {
        DeviceIdentity {
            secret: SecretKey::generate(),
        }
    }

    pub fn device_id(&self) -> DeviceId {
        DeviceId(self.secret.public().to_string())
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.secret.public()
    }

    pub fn secret_key(&self) -> &SecretKey {
        &self.secret
    }
}

/// Parse a `DeviceId` (hex or z32) into an iroh endpoint id.
pub fn endpoint_id(d: &DeviceId) -> Result<EndpointId> {
    EndpointId::from_str(&d.0).map_err(|e| Error::Invalid(format!("bad device id {:?}: {e}", d.0)))
}

pub fn device_id(e: &EndpointId) -> DeviceId {
    DeviceId(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::mock::MemSecretStore;

    #[test]
    fn identity_is_stable_and_matches_endpoint_id() {
        let s = MemSecretStore::default();
        let a = DeviceIdentity::load_or_create(&s).unwrap();
        let b = DeviceIdentity::load_or_create(&s).unwrap();
        assert_eq!(a.device_id(), b.device_id());
        assert_eq!(a.device_id().0.len(), 64);
        assert_eq!(endpoint_id(&a.device_id()).unwrap(), a.endpoint_id());
        assert!(endpoint_id(&DeviceId("nope".into())).is_err());
    }
}
