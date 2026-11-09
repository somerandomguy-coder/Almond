//! The paired-device allowlist ("vault"), persisted in the store's local-only kv table.

use almond_types::store::Store;
use almond_types::sync::DeviceInfo;
use almond_types::time::{UnixMs, now_ms};
use almond_types::{DeviceId, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

const KV_KEY: &str = "sync.paired_peers";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedPeer {
    pub info: DeviceInfo,
    /// Last known socket addresses ("192.168.1.20:47321").
    pub addrs: Vec<String>,
    pub paired_at: UnixMs,
    pub last_seen: Option<UnixMs>,
    pub last_synced: Option<UnixMs>,
}

pub struct PeerBook {
    store: Arc<dyn Store>,
    peers: RwLock<BTreeMap<DeviceId, PairedPeer>>,
}

impl PeerBook {
    pub fn load(store: Arc<dyn Store>) -> Result<Self> {
        let peers = match store.kv_get(KV_KEY)? {
            Some(s) => serde_json::from_str(&s)?,
            None => BTreeMap::new(),
        };
        Ok(PeerBook {
            store,
            peers: RwLock::new(peers),
        })
    }

    fn save(&self, m: &BTreeMap<DeviceId, PairedPeer>) -> Result<()> {
        self.store.kv_set(KV_KEY, &serde_json::to_string(m)?)
    }

    fn update(&self, f: impl FnOnce(&mut BTreeMap<DeviceId, PairedPeer>) -> bool) -> Result<()> {
        let mut g = self.peers.write().unwrap_or_else(|p| p.into_inner());
        if f(&mut g) {
            self.save(&g)?;
        }
        Ok(())
    }

    pub fn is_paired(&self, d: &DeviceId) -> bool {
        self.peers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .contains_key(d)
    }

    pub fn get(&self, d: &DeviceId) -> Option<PairedPeer> {
        self.peers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .get(d)
            .cloned()
    }

    pub fn list(&self) -> Vec<PairedPeer> {
        self.peers
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .values()
            .cloned()
            .collect()
    }

    pub fn add(&self, info: DeviceInfo, addrs: Vec<String>) -> Result<()> {
        self.update(|m| {
            let now = now_ms();
            let e = m.entry(info.id.clone()).or_insert_with(|| PairedPeer {
                info: info.clone(),
                addrs: vec![],
                paired_at: now,
                last_seen: Some(now),
                last_synced: None,
            });
            e.info = info;
            merge_addrs(&mut e.addrs, addrs);
            true
        })
    }

    pub fn remove(&self, d: &DeviceId) -> Result<bool> {
        let mut removed = false;
        self.update(|m| {
            removed = m.remove(d).is_some();
            removed
        })?;
        Ok(removed)
    }

    /// Remember addresses for a paired peer (no-op for unpaired ids).
    pub fn note_addrs(&self, d: &DeviceId, addrs: Vec<String>) -> Result<()> {
        self.update(|m| match m.get_mut(d) {
            Some(p) => {
                let before = p.addrs.clone();
                merge_addrs(&mut p.addrs, addrs);
                p.addrs != before
            }
            None => false,
        })
    }

    /// Refresh the stored `DeviceInfo` (e.g. renamed device, new capabilities) for a paired peer.
    pub fn note_info(&self, info: &DeviceInfo) -> Result<()> {
        self.update(|m| match m.get_mut(&info.id) {
            Some(p) if p.info != *info => {
                p.info = info.clone();
                true
            }
            _ => false,
        })
    }

    pub fn note_synced(&self, d: &DeviceId) -> Result<()> {
        self.update(|m| match m.get_mut(d) {
            Some(p) => {
                let now = now_ms();
                p.last_synced = Some(now);
                p.last_seen = Some(now);
                true
            }
            None => false,
        })
    }
}

/// Most recent first, de-duplicated, capped.
fn merge_addrs(cur: &mut Vec<String>, new: Vec<String>) {
    for a in new.into_iter().rev() {
        cur.retain(|x| *x != a);
        cur.insert(0, a);
    }
    cur.truncate(8);
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::mock::{MemStore, fixtures};

    #[test]
    fn persists_in_kv() {
        let store: Arc<dyn Store> = Arc::new(MemStore::new(DeviceId("me".into())));
        let b = PeerBook::load(store.clone()).unwrap();
        let pc = fixtures::device("pc");
        assert!(!b.is_paired(&pc.id));
        b.add(pc.clone(), vec!["10.0.0.2:1".into()]).unwrap();
        b.note_addrs(&pc.id, vec!["10.0.0.3:1".into(), "10.0.0.2:1".into()])
            .unwrap();
        b.note_synced(&pc.id).unwrap();
        let b2 = PeerBook::load(store).unwrap();
        let p = b2.get(&pc.id).unwrap();
        assert_eq!(p.addrs, vec!["10.0.0.3:1", "10.0.0.2:1"]);
        assert!(p.last_synced.is_some());
        assert!(b2.remove(&pc.id).unwrap());
        assert!(!b2.is_paired(&pc.id));
    }
}
