//! On-demand sync sessions: `sync_now` (initiator), `serve` (responder), auto-sync while open.

use crate::protocol::{self, ChunkHeader, Frame, Msg, PROTO_VERSION};
use almond_store::{FsBlobStore, SqliteStore};
use almond_types::store::{BlobStore, Store};
use almond_types::sync::{
    Connection, DeviceInfo, SyncOutcome, SyncReport, Transport, VersionVector,
};
use almond_types::time::now_ms;
use almond_types::{BlobHash, DeviceId, Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Which blobs to fetch automatically during a sync. Explicit requests
/// ([`SyncEngine::request_blob`]) are always honored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlobPolicy {
    /// PC: fetch every announced blob.
    Everything,
    /// Phone: metadata + text always; audio/video only on demand.
    TextOnly,
    /// Never fetch automatically.
    OnDemand,
}

impl BlobPolicy {
    pub fn auto_fetch(&self, mime: &str) -> bool {
        match self {
            BlobPolicy::Everything => true,
            BlobPolicy::TextOnly => !(mime.starts_with("audio/") || mime.starts_with("video/")),
            BlobPolicy::OnDemand => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct SyncOptions {
    pub blob_policy: BlobPolicy,
    /// Max events per `Events` message.
    pub batch_events: u32,
    /// Soft cap on serialized bytes per `Events` message (always ≥ 1 event).
    pub batch_bytes: usize,
    /// Blob chunk size.
    pub blob_chunk: u32,
    /// Per-message receive timeout.
    pub io_timeout: Duration,
    pub connect_timeout: Duration,
}

impl Default for SyncOptions {
    fn default() -> Self {
        SyncOptions {
            blob_policy: BlobPolicy::Everything,
            batch_events: 256,
            batch_bytes: 2 << 20,
            blob_chunk: 256 << 10,
            io_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(15),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SyncProgress {
    Started {
        peer: DeviceId,
    },
    Events {
        peer: DeviceId,
        received: u32,
        sent: u32,
    },
    Blob {
        peer: DeviceId,
        hash: BlobHash,
        bytes: u64,
        total: u64,
    },
    Finished(SyncReport),
}

pub type ProgressFn = Arc<dyn Fn(&SyncProgress) + Send + Sync>;

#[derive(Default, Debug, Clone, Copy)]
struct Stats {
    sent: u32,
    received: u32,
    blobs_sent: u32,
    blobs_received: u32,
}

enum End {
    Turn,
    Bye,
}

pub struct SyncEngine {
    store: Arc<SqliteStore>,
    blobs: Arc<FsBlobStore>,
    transport: Arc<dyn Transport>,
    device: DeviceInfo,
    opts: SyncOptions,
    requested: Mutex<BTreeSet<BlobHash>>,
    changed: Notify,
}

impl SyncEngine {
    pub fn new(
        store: Arc<SqliteStore>,
        blobs: Arc<FsBlobStore>,
        transport: Arc<dyn Transport>,
        device: DeviceInfo,
        opts: SyncOptions,
    ) -> Arc<Self> {
        Arc::new(SyncEngine {
            store,
            blobs,
            transport,
            device,
            opts,
            requested: Mutex::new(BTreeSet::new()),
            changed: Notify::new(),
        })
    }

    pub fn store(&self) -> &Arc<SqliteStore> {
        &self.store
    }

    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    /// Ask for a blob (e.g. audio on the phone) to be fetched on the next sync.
    pub fn request_blob(&self, hash: BlobHash) {
        self.requested
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(hash);
    }

    /// Wake the auto-sync loop early (call after local edits).
    pub fn notify_local_change(&self) {
        self.changed.notify_one();
    }

    // ------------------------------------------------------------------ initiator

    /// Sync with the given peers, or every paired peer currently visible. Peers are synced one
    /// after another; if a later peer delivered new events, earlier peers get a second pass so
    /// one press of "Sync" relays changes across all reachable devices.
    pub async fn sync_now(
        &self,
        peers: Option<Vec<DeviceId>>,
        progress: Option<ProgressFn>,
    ) -> Vec<SyncReport> {
        let targets = match peers {
            Some(p) => p,
            None => match self.transport.discover().await {
                Ok(v) => v
                    .into_iter()
                    .filter(|p| p.paired && p.device.id != self.device.id)
                    .map(|p| p.device.id)
                    .collect(),
                Err(e) => {
                    tracing::warn!("discover failed: {e}");
                    vec![]
                }
            },
        };
        let mut reports = Vec::with_capacity(targets.len());
        for p in &targets {
            reports.push(self.sync_with(p, progress.clone()).await);
        }
        let last_new = reports
            .iter()
            .rposition(|r| r.error.is_none() && r.events_received > 0);
        if let Some(k) = last_new
            && k > 0
        {
            for i in 0..k {
                if reports[i].error.is_some() {
                    continue;
                }
                let r2 = self.sync_with(&targets[i], progress.clone()).await;
                let r = &mut reports[i];
                r.events_sent += r2.events_sent;
                r.events_received += r2.events_received;
                r.blobs_sent += r2.blobs_sent;
                r.blobs_received += r2.blobs_received;
                r.duration_ms += r2.duration_ms;
                r.error = r2.error;
            }
        }
        reports
    }

    /// One session with one peer.
    pub async fn sync_with(&self, peer: &DeviceId, progress: Option<ProgressFn>) -> SyncReport {
        let t0 = Instant::now();
        let emit = |p: SyncProgress| {
            if let Some(f) = &progress {
                f(&p)
            }
        };
        emit(SyncProgress::Started { peer: peer.clone() });
        let mut stats = Stats::default();
        let res = async {
            let mut conn =
                tokio::time::timeout(self.opts.connect_timeout, self.transport.connect(peer))
                    .await
                    .map_err(|_| Error::Network(format!("connect to {peer} timed out")))??;
            let r = self
                .run_initiator(conn.as_mut(), &mut stats, &progress)
                .await;
            if let Err(e) = &r {
                let _ = self
                    .send(
                        conn.as_mut(),
                        Msg::Error {
                            message: e.to_string(),
                        },
                    )
                    .await;
            }
            let _ = conn.close().await;
            r
        }
        .await;
        if res.is_ok() {
            let _ = self.store.kv_set(
                &format!("sync.last_synced.{}", peer.0),
                &now_ms().to_string(),
            );
        }
        let report = SyncReport {
            peer: peer.clone(),
            events_sent: stats.sent,
            events_received: stats.received,
            blobs_sent: stats.blobs_sent,
            blobs_received: stats.blobs_received,
            duration_ms: t0.elapsed().as_millis() as u32,
            status: match &res {
                Ok(_) => SyncOutcome::Ok,
                Err(Error::Network(_)) => SyncOutcome::Offline,
                Err(_) => SyncOutcome::Failed,
            },
            error: res.err().map(|e| e.to_string()),
        };
        emit(SyncProgress::Finished(report.clone()));
        report
    }

    async fn run_initiator(
        &self,
        conn: &mut dyn Connection,
        s: &mut Stats,
        progress: &Option<ProgressFn>,
    ) -> Result<()> {
        self.send(conn, self.hello()?).await?;
        self.expect_hello(conn).await?;
        self.drive(conn, s, progress).await?;
        self.send(conn, Msg::Turn).await?;
        match self.serve_requests(conn, s, progress).await? {
            End::Turn => self.send(conn, Msg::Bye).await,
            End::Bye => Err(Error::Invalid("peer ended session early".into())),
        }
    }

    // ------------------------------------------------------------------ responder

    /// Accept and serve incoming sessions until the transport shuts down.
    pub async fn serve(self: Arc<Self>) {
        loop {
            match self.transport.accept().await {
                Ok(conn) => {
                    let me = self.clone();
                    tokio::spawn(async move {
                        let peer = conn.peer().clone();
                        if let Err(e) = me.handle_incoming(conn).await {
                            tracing::warn!("sync session from {peer} failed: {e}");
                        }
                    });
                }
                Err(e) => {
                    tracing::info!("sync server stopped: {e}");
                    return;
                }
            }
        }
    }

    pub fn spawn_server(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(self.clone().serve())
    }

    /// Serve one incoming session (responder role).
    pub async fn handle_incoming(&self, mut conn: Box<dyn Connection>) -> Result<SyncReport> {
        let t0 = Instant::now();
        let mut s = Stats::default();
        let r = self.run_responder(conn.as_mut(), &mut s).await;
        if let Err(e) = &r {
            let _ = self
                .send(
                    conn.as_mut(),
                    Msg::Error {
                        message: e.to_string(),
                    },
                )
                .await;
        }
        let _ = conn.close().await;
        if r.is_ok() && s.received > 0 {
            self.changed.notify_one();
        }
        r.map(|_| SyncReport {
            peer: conn.peer().clone(),
            events_sent: s.sent,
            events_received: s.received,
            blobs_sent: s.blobs_sent,
            blobs_received: s.blobs_received,
            duration_ms: t0.elapsed().as_millis() as u32,
            error: None,
            status: SyncOutcome::Ok,
        })
    }

    async fn run_responder(&self, conn: &mut dyn Connection, s: &mut Stats) -> Result<()> {
        self.expect_hello(conn).await?;
        self.send(conn, self.hello()?).await?;
        match self.serve_requests(conn, s, &None).await? {
            End::Turn => {}
            End::Bye => return Ok(()),
        }
        self.drive(conn, s, &None).await?;
        self.send(conn, Msg::Turn).await?;
        match self.serve_requests(conn, s, &None).await? {
            End::Bye => Ok(()),
            End::Turn => Err(Error::Invalid("unexpected second turn".into())),
        }
    }

    // ------------------------------------------------------------------ shared

    fn hello(&self) -> Result<Msg> {
        Ok(Msg::Hello {
            proto: PROTO_VERSION,
            device: self.device.clone(),
            vv: self.store.version_vector()?,
        })
    }

    async fn expect_hello(&self, conn: &mut dyn Connection) -> Result<DeviceInfo> {
        match self.recv(conn).await? {
            Frame::Msg(Msg::Hello { proto, device, .. }) => {
                if proto != PROTO_VERSION {
                    return Err(Error::Unsupported(format!(
                        "peer speaks sync protocol v{proto}, we speak v{PROTO_VERSION}"
                    )));
                }
                if &device.id != conn.peer() {
                    return Err(Error::PermissionDenied(format!(
                        "peer claims {} but authenticated as {}",
                        device.id,
                        conn.peer()
                    )));
                }
                Ok(device)
            }
            f => Err(unexpected(&f)),
        }
    }

    /// Pull everything we're missing from the peer: events, then wanted blobs.
    async fn drive(
        &self,
        conn: &mut dyn Connection,
        s: &mut Stats,
        progress: &Option<ProgressFn>,
    ) -> Result<()> {
        let peer = conn.peer().clone();
        let emit = |p: SyncProgress| {
            if let Some(f) = progress {
                f(&p)
            }
        };
        let mut cursor: VersionVector = self.store.version_vector()?;
        loop {
            self.send(
                conn,
                Msg::Pull {
                    have: cursor.clone(),
                    limit: self.opts.batch_events,
                },
            )
            .await?;
            let events = match self.recv(conn).await? {
                Frame::Msg(Msg::Events { events }) => events,
                f => return Err(unexpected(&f)),
            };
            if events.is_empty() {
                break;
            }
            for e in &events {
                cursor.observe(&e.device, e.seq);
            }
            let st = self.store.clone();
            let n = tokio::task::spawn_blocking(move || st.apply_remote(events))
                .await
                .map_err(Error::other)??;
            s.received += n as u32;
            emit(SyncProgress::Events {
                peer: peer.clone(),
                received: s.received,
                sent: s.sent,
            });
        }

        for hash in self.wanted_blobs()? {
            match self.fetch_blob(conn, &hash, &emit).await {
                Ok(true) => {
                    s.blobs_received += 1;
                    self.requested
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&hash);
                }
                Ok(false) => {}
                Err(e @ Error::Invalid(_)) => {
                    tracing::warn!("blob {} from {peer}: {e}", hash.0);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn wanted_blobs(&self) -> Result<Vec<BlobHash>> {
        let mut out: BTreeSet<BlobHash> = self
            .requested
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        for b in self.store.known_blobs()? {
            if self.opts.blob_policy.auto_fetch(&b.mime) {
                out.insert(b.hash);
            }
        }
        let mut v = vec![];
        for h in out {
            if almond_store::blobs::validate_hash(&h).is_ok() && !self.blobs.has(&h)? {
                v.push(h);
            }
        }
        Ok(v)
    }

    /// Fetch one blob (resuming a partial download). `Ok(false)` = peer doesn't have it.
    async fn fetch_blob(
        &self,
        conn: &mut dyn Connection,
        hash: &BlobHash,
        emit: &impl Fn(SyncProgress),
    ) -> Result<bool> {
        let mut off = self.blobs.partial_len(hash)?;
        loop {
            self.send(
                conn,
                Msg::BlobGet {
                    hash: hash.clone(),
                    offset: off,
                    max: self.opts.blob_chunk,
                },
            )
            .await?;
            match self.recv(conn).await? {
                Frame::Msg(Msg::BlobMissing { .. }) => return Ok(false),
                Frame::Chunk(h, data) => {
                    if &h.hash != hash || h.offset != off {
                        return Err(Error::Network("blob chunk out of order".into()));
                    }
                    if off > h.total || (data.is_empty() && off < h.total) {
                        // stale/corrupt partial: start over next time
                        self.blobs.discard_partial(hash)?;
                        return Err(Error::Invalid("partial larger than blob".into()));
                    }
                    if off + data.len() as u64 > h.total {
                        return Err(Error::Network("blob chunk past end".into()));
                    }
                    self.blobs.write_partial(hash, off, &data)?;
                    off += data.len() as u64;
                    emit(SyncProgress::Blob {
                        peer: conn.peer().clone(),
                        hash: hash.clone(),
                        bytes: off,
                        total: h.total,
                    });
                    if off == h.total {
                        self.blobs.finish_partial(hash)?;
                        return Ok(true);
                    }
                }
                f => return Err(unexpected(&f)),
            }
        }
    }

    /// Answer the peer's requests until it hands over (`Turn`) or ends (`Bye`).
    async fn serve_requests(
        &self,
        conn: &mut dyn Connection,
        s: &mut Stats,
        progress: &Option<ProgressFn>,
    ) -> Result<End> {
        loop {
            match self.recv(conn).await? {
                Frame::Msg(Msg::Pull { have, limit }) => {
                    let limit = limit.clamp(1, 4096) as usize;
                    let st = self.store.clone();
                    let mut events =
                        tokio::task::spawn_blocking(move || st.events_missing(&have, limit))
                            .await
                            .map_err(Error::other)??;
                    // soft byte cap; dropping a suffix keeps (device, seq) order intact
                    let mut bytes = 0usize;
                    let mut keep = events.len();
                    for (i, e) in events.iter().enumerate() {
                        bytes += serde_json::to_vec(&e.body).map(|v| v.len()).unwrap_or(0) + 128;
                        if bytes > self.opts.batch_bytes && i > 0 {
                            keep = i;
                            break;
                        }
                    }
                    events.truncate(keep);
                    s.sent += events.len() as u32;
                    self.send(conn, Msg::Events { events }).await?;
                    if let Some(f) = progress {
                        f(&SyncProgress::Events {
                            peer: conn.peer().clone(),
                            received: s.received,
                            sent: s.sent,
                        });
                    }
                }
                Frame::Msg(Msg::BlobGet { hash, offset, max }) => {
                    let max = max.clamp(1, 4 << 20) as usize;
                    let frame = match self.blobs.size(&hash) {
                        Ok(Some(total)) => {
                            let data = self
                                .blobs
                                .read_range(&hash, offset, max)?
                                .unwrap_or_default();
                            if offset + data.len() as u64 == total && !data.is_empty() {
                                s.blobs_sent += 1;
                            }
                            Frame::Chunk(
                                ChunkHeader {
                                    hash,
                                    offset,
                                    total,
                                },
                                data,
                            )
                        }
                        _ => Frame::Msg(Msg::BlobMissing { hash }),
                    };
                    self.send_frame(conn, &frame).await?;
                }
                Frame::Msg(Msg::Turn) => return Ok(End::Turn),
                Frame::Msg(Msg::Bye) => return Ok(End::Bye),
                f => return Err(unexpected(&f)),
            }
        }
    }

    async fn send(&self, conn: &mut dyn Connection, m: Msg) -> Result<()> {
        self.send_frame(conn, &Frame::Msg(m)).await
    }

    async fn send_frame(&self, conn: &mut dyn Connection, f: &Frame) -> Result<()> {
        conn.send(protocol::encode(f)?).await
    }

    async fn recv(&self, conn: &mut dyn Connection) -> Result<Frame> {
        let b = tokio::time::timeout(self.opts.io_timeout, conn.recv())
            .await
            .map_err(|_| Error::Network("sync peer timed out".into()))??
            .ok_or_else(|| Error::Network("peer closed the connection".into()))?;
        if b.len() > protocol::MAX_FRAME {
            return Err(Error::Invalid("sync frame too large".into()));
        }
        match protocol::decode(&b)? {
            Frame::Msg(Msg::Error { message }) => Err(Error::Network(format!("peer: {message}"))),
            f => Ok(f),
        }
    }

    // ------------------------------------------------------------------ auto-sync

    /// While the app is open: sync with all visible paired peers every `interval`, and shortly
    /// after local changes ([`Self::notify_local_change`]). Dropping the handle stops it.
    pub fn start_auto_sync(
        self: &Arc<Self>,
        interval: Duration,
        progress: Option<ProgressFn>,
    ) -> AutoSyncHandle {
        let me = self.clone();
        AutoSyncHandle(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = me.changed.notified() => {
                        // debounce bursts of edits
                        tokio::time::sleep(Duration::from_millis(1500)).await;
                    }
                }
                let _ = me.sync_now(None, progress.clone()).await;
            }
        }))
    }
}

/// Stops auto-sync when dropped.
pub struct AutoSyncHandle(tokio::task::JoinHandle<()>);

impl Drop for AutoSyncHandle {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn unexpected(f: &Frame) -> Error {
    let what = match f {
        Frame::Msg(m) => format!("{m:?}"),
        Frame::Chunk(h, _) => format!("chunk of {}", h.hash.0),
    };
    let what: String = what.chars().take(80).collect();
    Error::Invalid(format!("unexpected sync message: {what}"))
}
