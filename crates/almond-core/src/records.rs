//! Typed access to synced records (`record.upsert` / `record.delete` events, see
//! `almond_types::store::Record`). People, recipes and chat threads are stored this way.

use crate::Core;
use almond_types::Result;
use almond_types::event::EventBody;
use almond_types::store::Store;
use serde::Serialize;
use serde::de::DeserializeOwned;

impl Core {
    /// Write (create or replace) a record; it syncs to every paired device.
    pub fn put_record<T: Serialize>(&self, collection: &str, id: &str, value: &T) -> Result<()> {
        self.store().append_local(EventBody::RecordUpsert {
            collection: collection.into(),
            id: id.into(),
            body: serde_json::to_value(value)?,
        })?;
        Ok(())
    }

    /// Delete a record (a synced tombstone).
    pub fn delete_record(&self, collection: &str, id: &str) -> Result<()> {
        self.store().append_local(EventBody::RecordDelete {
            collection: collection.into(),
            id: id.into(),
        })?;
        Ok(())
    }

    /// One record, decoded. `None` if missing or deleted.
    pub fn record<T: DeserializeOwned>(&self, collection: &str, id: &str) -> Result<Option<T>> {
        match self.store().get_record(collection, id)? {
            Some(r) => Ok(Some(serde_json::from_value(r.body)?)),
            None => Ok(None),
        }
    }

    /// All records of a collection that decode as `T` (others, e.g. from a newer app version
    /// with an incompatible shape, are skipped with a warning).
    pub fn records<T: DeserializeOwned>(&self, collection: &str) -> Result<Vec<T>> {
        Ok(self
            .store()
            .list_records(collection)?
            .into_iter()
            .filter_map(|r| match serde_json::from_value(r.body) {
                Ok(v) => Some(v),
                Err(e) => {
                    tracing::warn!("record {collection}/{}: {e}", r.id);
                    None
                }
            })
            .collect())
    }
}
