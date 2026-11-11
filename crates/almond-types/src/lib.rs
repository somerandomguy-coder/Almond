//! # almond-types — the shared contract
//!
//! Every other crate depends on this one. It holds the domain model (meetings, artifacts,
//! events), the plugin traits each workstream implements, and the local API types.
//!
//! **Change policy:** this crate is frozen at tag `m0-contracts`. Workstreams that need a change
//! write a request in `docs/contract-requests/<ws>.md`; changes land as small PRs that everyone
//! rebases on. Prefer *adding* (new artifact kinds, new optional fields) over changing.
//!
//! Enable the `mock` feature to get in-memory implementations of every trait (see [`mock`]).

pub mod actions;
pub mod api;
pub mod artifact;
pub mod audio;
pub mod calendar;
pub mod chat;
pub mod error;
pub mod event;
pub mod ids;
pub mod job;
pub mod lang;
pub mod llm;
pub mod mail;
pub mod meeting;
pub mod people;
pub mod pipeline;
pub mod recipes;
pub mod secrets;
pub mod store;
pub mod stt;
pub mod sync;
pub mod time;
pub mod verify;

#[cfg(feature = "mock")]
pub mod mock;

pub use artifact::{Artifact, ArtifactDraft, ArtifactKind, Provenance, SpanRef, params_hash};
pub use error::{Error, Result};
pub use event::{Event, EventKind};
pub use ids::{BlobHash, DeviceId, Id};
pub use lang::LangTag;
pub use meeting::Meeting;
pub use time::{Hlc, HlcClock, UnixMs};
