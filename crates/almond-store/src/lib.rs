//! # almond-store — local persistence (WS-D)
//!
//! - [`SqliteStore`]: the replicated event log + materialized views (meetings, artifacts, notes,
//!   calendar, jobs, devices, blobs) + FTS5 search with Vietnamese diacritic folding.
//! - [`FsBlobStore`]: content-addressed (BLAKE3) files with streaming writes and resumable
//!   partial downloads.
//! - Loro notes helpers ([`notes_update_event`], [`SqliteStore::notes_doc`],
//!   [`SqliteStore::compact_notes`]).
//!
//! ## Convergence rules (all order-independent)
//! | view | rule |
//! |---|---|
//! | meetings | LWW register per id over `meeting.upsert`/`meeting.delete`, ordered by `(hlc, device)`; delete = tombstone |
//! | artifacts | immutable, insert-once |
//! | notes | Loro updates (CRDT merge) |
//! | calendar | LWW per `(source, event_id)` with tombstones |
//! | jobs | status derived at read time: latest outcome › active (unexpired) lowest claim › pending |
//! | devices | LWW per device id |
//! | unknown kinds | stored + relayed, never materialized |
//!
//! The version vector only advances over contiguous sequence numbers, so out-of-order or
//! partial batches never cause gaps to be skipped.

pub mod blobs;
pub mod fts;
pub mod notes;
pub mod schema;
pub mod sqlite;

pub use almond_types as types;
pub use blobs::FsBlobStore;
pub use notes::{doc_text, export_since, notes_update_event, validate_update};
pub use sqlite::{BlobInfo, SqliteStore};

#[cfg(test)]
mod tests;
