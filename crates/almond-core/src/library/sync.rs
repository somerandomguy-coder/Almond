//! `sync.*` over the core's `SyncNode` (iroh transport + sync engine), auto-sync while the app
//! is open, and this device's activity heartbeat (`device.info` with `last_active`).

use super::add;
use crate::Core;
use crate::error::ApiResult;
use crate::router::{NoParams, Router};
use crate::settings::keys;
use almond_sync::{PairingSecret, ticket_from_qr, ticket_to_qr};
use almond_types::api::{PairBeginResult, SyncProgress, SyncStatus, notifications};
use almond_types::event::EventBody;
use almond_types::store::Store;
use almond_types::sync::{PeerInfo, SyncReport, Transport};
use almond_types::time::now_ms;
use almond_types::{DeviceId, Error, Result};
use serde::Deserialize;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Auto-sync period while the app is open (setting `sync.auto_while_open`).
const AUTO_SYNC_EVERY: Duration = Duration::from_secs(120);
/// How long a pairing code stays valid.
const PAIRING_TTL: Duration = Duration::from_secs(5 * 60);
/// Re-announce `device.info { last_active }` at most this often while the app is used.
const HEARTBEAT_EVERY_MS: i64 = 3 * 60_000;

#[derive(Default)]
pub(crate) struct SyncState {
    syncing: AtomicBool,
    lock: tokio::sync::Mutex<()>,
    last_reports: Mutex<Vec<SyncReport>>,
    last_heartbeat: AtomicI64,
}

/// User activity: feeds the reminder policy and, every few minutes, a `device.info` heartbeat
/// so peers know which device is in use.
pub(super) fn touch(core: &Core) {
    let now = now_ms();
    core.library.activity.record(core.device_id().clone(), now);
    let hb = &core.library.sync.last_heartbeat;
    let last = hb.load(Ordering::Relaxed);
    if now - last >= HEARTBEAT_EVERY_MS
        && hb
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_ok()
    {
        let mut me = core.device().clone();
        me.last_active = Some(now);
        if let Err(e) = core.store().append_local(EventBody::DeviceInfo(me)) {
            tracing::warn!("device.info heartbeat: {e}");
        }
    }
}

fn progress_payload(p: &almond_sync::SyncProgress) -> SyncProgress {
    use almond_sync::SyncProgress as P;
    let kib = |b: u64| (b / 1024).min(u32::MAX as u64) as u32;
    match p {
        P::Started { peer } => SyncProgress {
            peer: peer.clone(),
            phase: "connecting".into(),
            done: 0,
            total: 0,
        },
        P::Events {
            peer,
            received,
            sent,
        } => SyncProgress {
            peer: peer.clone(),
            phase: "events".into(),
            done: received + sent,
            total: 0,
        },
        P::Blob {
            peer, bytes, total, ..
        } => SyncProgress {
            peer: peer.clone(),
            phase: "blobs".into(),
            done: kib(*bytes),
            total: kib(*total),
        },
        P::Finished(r) => SyncProgress {
            peer: r.peer.clone(),
            phase: if r.error.is_none() { "done" } else { "error" }.into(),
            done: r.events_received + r.events_sent,
            total: r.events_received + r.events_sent,
        },
    }
}

/// Sync with the given peers, or every paired peer. One sync at a time; a second call waits.
pub(crate) async fn run_sync(core: &Arc<Core>, peers: Option<Vec<DeviceId>>) -> Vec<SyncReport> {
    let st = &core.library.sync;
    let _g = st.lock.lock().await;
    st.syncing.store(true, Ordering::SeqCst);
    let notifier = core.notifier.clone();
    let progress: almond_sync::ProgressFn = Arc::new(move |p| {
        notifier.emit(notifications::SYNC_PROGRESS, progress_payload(p));
    });
    let reports = core.node.engine.sync_now(peers, Some(progress)).await;
    let book = core.node.transport.peer_book();
    for r in reports.iter().filter(|r| r.error.is_none()) {
        let _ = book.note_synced(&r.peer);
    }
    if reports.iter().any(|r| r.events_received > 0) {
        // synced calendar events / device activity may change what is due
        super::calendar::wake_reminders(core);
    }
    {
        let mut last = st.last_reports.lock().unwrap_or_else(|p| p.into_inner());
        for r in &reports {
            last.retain(|x| x.peer != r.peer);
            last.push(r.clone());
        }
    }
    st.syncing.store(false, Ordering::SeqCst);
    reports
}

fn auto_sync_on(core: &Core) -> bool {
    core.setting_as::<bool>(keys::SYNC_AUTO_WHILE_OPEN)
        .ok()
        .flatten()
        .unwrap_or(false)
}

/// Background: sync with all paired peers every 2 minutes while `sync.auto_while_open` is on.
pub(crate) async fn auto_sync_loop(core: Arc<Core>) {
    loop {
        tokio::time::sleep(AUTO_SYNC_EVERY).await;
        let has_peers = !core.node.transport.paired_peers().is_empty();
        if has_peers && auto_sync_on(&core) && !core.library.sync.syncing.load(Ordering::SeqCst) {
            run_sync(&core, None).await;
        }
    }
}

async fn status(core: &Core) -> Result<SyncStatus> {
    let mut me = core.device().clone();
    me.last_active = core
        .library
        .activity
        .snapshot()
        .get(core.device_id())
        .copied();
    let peers = core
        .node
        .transport
        .discover()
        .await?
        .into_iter()
        .filter(|p| &p.device.id != core.device_id())
        .collect();
    Ok(SyncStatus {
        this_device: me,
        peers,
        syncing: core.library.sync.syncing.load(Ordering::SeqCst),
        auto_sync_while_open: auto_sync_on(core),
        last_reports: core
            .library
            .sync
            .last_reports
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone(),
    })
}

fn pair_begin(core: &Core) -> Result<PairBeginResult> {
    let t = core.node.transport.start_pairing(PAIRING_TTL);
    let words = PairingSecret::from_base32(&t.secret)?
        .to_words()
        .split('-')
        .map(str::to_string)
        .collect();
    Ok(PairBeginResult {
        qr_payload: ticket_to_qr(&t),
        words,
        expires_at: t.expires_at,
    })
}

/// `words` as the UI sends them (an array) or as one string.
#[derive(Deserialize)]
#[serde(untagged)]
enum Words {
    List(Vec<String>),
    Text(String),
}

#[derive(Deserialize)]
struct PairAcceptParams {
    qr_payload: Option<String>,
    words: Option<Words>,
    /// Optional host device id for the 6 words (when mDNS can't see the host but its address
    /// was added with `sync.addAddr`).
    device: Option<DeviceId>,
}

async fn pair_accept(core: &Arc<Core>, p: PairAcceptParams) -> Result<PeerInfo> {
    let tr = &core.node.transport;
    let info = match (p.qr_payload, p.words) {
        (Some(qr), _) if !qr.trim().is_empty() => {
            tr.pair_with_ticket(&ticket_from_qr(qr.trim())?).await?
        }
        (_, Some(w)) => {
            let words = match w {
                Words::List(v) => v.join(" "),
                Words::Text(s) => s,
            };
            tr.pair_with_code(&words, p.device).await?
        }
        _ => return Err(Error::Invalid("give qr_payload or words".into())),
    };
    let peer = tr.peer_book().get(&info.id);
    // first sync right away, in the background
    let c = core.clone();
    let id = info.id.clone();
    tokio::spawn(async move {
        run_sync(&c, Some(vec![id])).await;
    });
    Ok(PeerInfo {
        device: info,
        addrs: peer.as_ref().map(|p| p.addrs.clone()).unwrap_or_default(),
        paired: true,
        last_seen: Some(now_ms()),
        last_synced: None,
    })
}

#[derive(Deserialize)]
struct SyncNowParams {
    peers: Option<Vec<DeviceId>>,
}

#[derive(Deserialize)]
struct DeviceParams {
    device: DeviceId,
}

#[derive(Deserialize)]
struct AddAddrParams {
    device: DeviceId,
    addr: String,
}

pub(super) fn register(r: &mut Router) {
    add(r, "sync.status", |core, _: NoParams| async move {
        ApiResult::Ok(status(&core).await?)
    });
    add(r, "sync.now", |core, p: SyncNowParams| async move {
        let peers = p.peers.filter(|v| !v.is_empty());
        ApiResult::Ok(run_sync(&core, peers).await)
    });
    add(r, "sync.pairBegin", |core, _: NoParams| async move {
        ApiResult::Ok(pair_begin(&core)?)
    });
    add(
        r,
        "sync.pairAccept",
        |core, p: PairAcceptParams| async move { ApiResult::Ok(pair_accept(&core, p).await?) },
    );
    add(r, "sync.unpair", |core, p: DeviceParams| async move {
        if !core.node.transport.unpair(&p.device)? {
            return Err(Error::NotFound(format!("paired device {}", p.device)).into());
        }
        ApiResult::Ok(Value::Null)
    });
    add(r, "sync.addAddr", |core, p: AddAddrParams| async move {
        core.node
            .transport
            .add_addr(p.device, p.addr.trim().to_string())
            .await?;
        ApiResult::Ok(Value::Null)
    });
}
