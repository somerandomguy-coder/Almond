# WS-D (store + sync) — contract notes and requests

None of these block WS-D; each one is worked around locally. They are listed for the integrator.

## 1. `Transport::add_addr(None, addr)` cannot work with iroh

iroh dials by public key: a bare `ip:port` without the device id cannot be authenticated, and it
cannot even be connected to. `IrohTransport::add_addr(None, _)` returns `Error::Invalid`.

**Proposal:** make the device id required (`add_addr(device: DeviceId, addr: String)`), or
document that `None` is only valid for transports that can learn the id (the mock can).
The UI's manual-address form should take the device id as well. The id is shown under
Settings → Devices, and is included in the pairing QR and the NSD TXT record.

## 2. Blob announcements are not exposed on `Store`

Lazy blob fetch needs the list of announced blobs (`blob.available` events). `SqliteStore` has
`known_blobs()`, but the `Store` trait does not. For that reason `SyncEngine` takes a concrete
`Arc<SqliteStore>` (plus `Arc<FsBlobStore>` for resumable partial downloads) rather than
`dyn Store` / `dyn BlobStore`.

**Proposal (additive):** add `fn known_blobs(&self) -> Result<Vec<BlobInfo>>` to `Store`. Also add
optional range and partial methods to `BlobStore`. The engine could then be fully trait-based.

## 3. Producers must announce blobs

Only blobs announced with `EventBody::BlobAvailable { hash, size, mime }` get synced. Whoever
writes `Meeting.audio` tracks (WS-A recording, or the integration layer) must also append a
`blob.available` event with a meaningful `mime` (`audio/ogg`, ...). Per-device policy relies on
the mime: phones use `BlobPolicy::TextOnly`, so audio is skipped unless requested with
`SyncEngine::request_blob`.

## 4. Semantics to document in `almond-types`

These are what the real store implements. The mock differs on the first two points.

- `meeting.delete` and `calendar.delete` are **LWW tombstones** ordered by `(hlc, device)`. An
  older upsert that arrives later does not resurrect the record, and a newer upsert does. The
  mock deletes outright, which is order-dependent.
- Job status is **derived at read time**:
  1. Latest `job.finished` by `(hlc, device)`.
  2. Otherwise `Claimed` if there is an unexpired claim; the winning lease is the lowest
     `(claimed_at, device)` (`SqliteStore::job_lease`).
  3. Otherwise `Pending`, including after a lease expires.

  Claims that arrive before the request still count.
- `Store::notes_updates` returns `[snapshot, newer updates…]` after
  `SqliteStore::compact_notes`. Loro import is order- and duplicate-insensitive, so consumers
  should simply import all of them (`SqliteStore::notes_doc` does this).
- `events_missing` may return non-contiguous events (gap-buffered ones). The receiver's version
  vector only advances over contiguous seqs.

## 5. `SyncReport` has no "skipped"/"offline" distinction

Unreachable peers show up as `error: Some("network: …")`. A typed `status` field would let the UI
show "offline" differently from real failures. This is optional.
