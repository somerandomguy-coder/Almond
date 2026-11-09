//! Sync contract (WS-D). On-demand LAN sync between paired devices; no daemon.

use crate::ids::DeviceId;
use crate::job::Capability;
use crate::time::UnixMs;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Highest contiguous `seq` seen per author device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, Default)]
#[ts(export)]
pub struct VersionVector(#[ts(type = "Record<string, number>")] pub BTreeMap<DeviceId, u64>);

impl VersionVector {
    pub fn get(&self, d: &DeviceId) -> u64 {
        self.0.get(d).copied().unwrap_or(0)
    }
    pub fn observe(&mut self, d: &DeviceId, seq: u64) {
        let e = self.0.entry(d.clone()).or_insert(0);
        if seq > *e {
            *e = seq;
        }
    }
    /// True if `self` has seen everything `other` has.
    pub fn dominates(&self, other: &VersionVector) -> bool {
        other.0.iter().all(|(d, s)| self.get(d) >= *s)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct DeviceInfo {
    pub id: DeviceId,
    /// User-facing name, e.g. "Nam's PC".
    pub name: String,
    /// "windows" | "linux" | "android"
    pub platform: String,
    pub app_version: String,
    pub capabilities: Vec<Capability>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PeerInfo {
    pub device: DeviceInfo,
    /// Socket addresses seen for this peer ("192.168.1.20:47321").
    pub addrs: Vec<String>,
    pub paired: bool,
    #[ts(type = "number | null")]
    pub last_seen: Option<UnixMs>,
    #[ts(type = "number | null")]
    pub last_synced: Option<UnixMs>,
}

/// Contents of the pairing QR code / 6-word code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct PairingTicket {
    pub device: DeviceInfo,
    pub addrs: Vec<String>,
    /// One-time secret (base32), valid for a few minutes.
    pub secret: String,
    #[ts(type = "number")]
    pub expires_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct SyncReport {
    pub peer: DeviceId,
    pub events_sent: u32,
    pub events_received: u32,
    pub blobs_sent: u32,
    pub blobs_received: u32,
    pub duration_ms: u32,
    pub error: Option<String>,
}

/// Bidirectional, ordered, reliable, authenticated message channel to a paired peer.
#[async_trait]
pub trait Connection: Send {
    fn peer(&self) -> &DeviceId;
    async fn send(&mut self, msg: Vec<u8>) -> crate::Result<()>;
    /// `None` = closed by peer.
    async fn recv(&mut self) -> crate::Result<Option<Vec<u8>>>;
    async fn close(&mut self) -> crate::Result<()>;
}

/// Network transport (iroh in production, in-process channels in `mock`).
#[async_trait]
pub trait Transport: Send + Sync {
    fn local_device(&self) -> DeviceId;
    /// Peers currently visible on the LAN (mDNS / NSD / manual), paired or not.
    async fn discover(&self) -> crate::Result<Vec<PeerInfo>>;
    /// Add a manually entered address ("192.168.1.20:47321").
    async fn add_addr(&self, device: Option<DeviceId>, addr: String) -> crate::Result<()>;
    async fn connect(&self, peer: &DeviceId) -> crate::Result<Box<dyn Connection>>;
    /// Wait for the next incoming connection from a *paired* peer.
    async fn accept(&self) -> crate::Result<Box<dyn Connection>>;
}
