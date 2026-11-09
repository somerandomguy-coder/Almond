//! # almond-sync — on-demand LAN sync (WS-D)
//!
//! No daemon: discovery and sync run only while the app is open. The user presses **Sync**
//! ([`SyncEngine::sync_now`]) or enables "auto-sync while open" ([`SyncEngine::start_auto_sync`]).
//!
//! - [`identity`]: ed25519 device key (= iroh endpoint id = `DeviceId`), kept in a `SecretStore`
//!   ([`secrets`]: OS keychain on Windows/macOS, 0600 file on Linux/Android).
//! - [`pairing`]: QR ticket / 6-word code, SPAKE2 + key confirmation; [`peers`] is the persisted
//!   allowlist enforced by the transport.
//! - [`iroh_transport`]: [`IrohTransport`], LAN-only iroh with mDNS + manual + NSD-injected
//!   addresses, behind the `almond_types::sync::Transport` trait.
//! - [`engine`] + [`protocol`]: version-vector replication of the event log, lazy blob fetch
//!   with per-device [`BlobPolicy`], resumable and idempotent, any number of peers.
//! - [`node`]: [`SyncNode`] wires all of the above from a data directory.

pub mod engine;
pub mod identity;
pub mod iroh_transport;
pub mod node;
pub mod pairing;
pub mod peers;
pub mod protocol;
pub mod secrets;
pub mod wordlist;

pub use almond_types as types;
pub use engine::{AutoSyncHandle, BlobPolicy, ProgressFn, SyncEngine, SyncOptions, SyncProgress};
pub use identity::DeviceIdentity;
pub use iroh_transport::{IrohConfig, IrohTransport, NsdAdvertisement};
pub use node::{NodeConfig, SyncNode};
pub use pairing::{PairingSecret, ticket_from_qr, ticket_to_qr};
pub use peers::{PairedPeer, PeerBook};
