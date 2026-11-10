//! Real iroh endpoints on 127.0.0.1 (in one process): pairing via QR ticket and 6-word code,
//! allowlist enforcement, event + blob sync with relaying through a hub.

use almond_store::{FsBlobStore, SqliteStore};
use almond_sync::{
    DeviceIdentity, IrohConfig, IrohTransport, PairingSecret, PeerBook, SyncEngine, SyncOptions,
    ticket_from_qr, ticket_to_qr,
};
use almond_types::event::EventBody;
use almond_types::mock::fixtures;
use almond_types::store::{BlobStore, MeetingQuery, Store};
use almond_types::sync::{DeviceInfo, Transport};
use std::sync::Arc;
use std::time::Duration;

struct Node {
    info: DeviceInfo,
    store: Arc<SqliteStore>,
    blobs: Arc<FsBlobStore>,
    transport: Arc<IrohTransport>,
    engine: Arc<SyncEngine>,
}

async fn node(name: &str, dir: &std::path::Path) -> Node {
    let id = DeviceIdentity::generate();
    let mut info = fixtures::device(name);
    info.id = id.device_id();
    let store =
        Arc::new(SqliteStore::open(dir.join(format!("{name}.db")), info.id.clone()).unwrap());
    let blobs = Arc::new(FsBlobStore::open(dir.join(format!("{name}-blobs"))).unwrap());
    let book = Arc::new(PeerBook::load(store.clone() as Arc<dyn Store>).unwrap());
    let cfg = IrohConfig {
        port: 0,
        bind_ip: Some("127.0.0.1".parse().unwrap()),
        mdns: false,
    };
    let transport = Arc::new(
        IrohTransport::bind(&id, info.clone(), book, cfg)
            .await
            .unwrap(),
    );
    let engine = SyncEngine::new(
        store.clone(),
        blobs.clone(),
        transport.clone() as Arc<dyn Transport>,
        info.clone(),
        SyncOptions {
            blob_chunk: 64 << 10,
            ..Default::default()
        },
    );
    engine.spawn_server();
    Node {
        info,
        store,
        blobs,
        transport,
        engine,
    }
}

fn titles(n: &Node) -> Vec<String> {
    let mut v: Vec<String> = n
        .store
        .list_meetings(&MeetingQuery::default())
        .unwrap()
        .into_iter()
        .map(|m| m.title)
        .collect();
    v.sort();
    v
}

fn add(n: &Node, title: &str) {
    let mut m = fixtures::meeting(1);
    m.title = title.into();
    n.store.append_local(EventBody::MeetingUpsert(m)).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pair_and_sync_over_real_iroh() {
    let dir = tempfile::tempdir().unwrap();
    let a = node("pc", dir.path()).await;
    let b = node("laptop", dir.path()).await;
    let c = node("phone", dir.path()).await;
    assert!(!a.transport.local_addrs().is_empty());

    // unpaired peers can't sync: locally refused...
    assert!(b.transport.connect(&a.info.id).await.is_err());
    // ...and refused by the remote even if *we* think we're paired
    b.transport
        .peer_book()
        .add(
            a.info.clone(),
            a.transport
                .local_addrs()
                .iter()
                .map(|x| x.to_string())
                .collect(),
        )
        .unwrap();
    let r = b.engine.sync_with(&a.info.id, None).await;
    assert!(
        r.error.is_some(),
        "remote must reject unpaired device: {r:?}"
    );
    b.transport.unpair(&a.info.id).unwrap();

    // pairing via QR ticket (A hosts, B scans)
    let mut paired_rx = a.transport.subscribe_paired();
    let ticket = a.transport.start_pairing(Duration::from_secs(60));
    let qr = ticket_to_qr(&ticket);
    let host = b
        .transport
        .pair_with_ticket(&ticket_from_qr(&qr).unwrap())
        .await
        .unwrap();
    assert_eq!(host.id, a.info.id);
    let joined = tokio::time::timeout(Duration::from_secs(5), paired_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(joined.id, b.info.id);
    // one-time: the same ticket can't be reused by C
    assert!(c.transport.pair_with_ticket(&ticket).await.is_err());

    // pairing via 6 words + manual address (A hosts again, C types the code)
    let t2 = a.transport.start_pairing(Duration::from_secs(60));
    let words = PairingSecret::from_base32(&t2.secret).unwrap().to_words();
    // wrong code first: fails, session stays open (failures < limit)
    let wrong = PairingSecret([0; 6]).to_words();
    c.transport
        .add_addr(a.info.id.clone(), t2.addrs[0].clone())
        .await
        .unwrap();
    assert!(
        c.transport
            .pair_with_code(&wrong, Some(a.info.id.clone()))
            .await
            .is_err()
    );
    c.transport
        .pair_with_code(&words, Some(a.info.id.clone()))
        .await
        .unwrap();
    assert!(a.transport.peer_book().is_paired(&c.info.id));
    assert!(c.transport.peer_book().is_paired(&a.info.id));
    assert!(!b.transport.peer_book().is_paired(&c.info.id));

    // offline edits on all three, a 1.5 MB "recording" on the laptop
    add(&a, "pc meeting");
    add(&b, "laptop meeting");
    add(&c, "phone meeting");
    let rec: Vec<u8> = (0..1_500_000u32).map(|i| (i % 253) as u8).collect();
    let h = b.blobs.put(&rec).unwrap();
    b.store
        .append_local(EventBody::BlobAvailable {
            hash: h.clone(),
            size: rec.len() as u64,
            mime: "audio/ogg".into(),
        })
        .unwrap();

    // B ⇄ A, C ⇄ A, then A presses Sync (relays B's data to C and C's to B)
    let rb = b.engine.sync_now(None, None).await;
    assert_eq!(rb.len(), 1, "B only sees A as paired: {rb:?}");
    assert!(rb[0].error.is_none(), "{rb:?}");
    let rc = c.engine.sync_now(None, None).await;
    assert!(rc[0].error.is_none(), "{rc:?}");
    let ra = a.engine.sync_now(None, None).await;
    assert_eq!(ra.len(), 2);
    assert!(ra.iter().all(|r| r.error.is_none()), "{ra:?}");

    let want = vec!["laptop meeting", "pc meeting", "phone meeting"];
    for n in [&a, &b, &c] {
        assert_eq!(titles(n), want, "{}", n.info.name);
        assert_eq!(
            n.blobs.get(&h).unwrap().as_deref(),
            Some(rec.as_slice()),
            "{}",
            n.info.name
        );
    }
    assert_eq!(
        a.store.version_vector().unwrap(),
        b.store.version_vector().unwrap()
    );
    assert_eq!(
        a.store.version_vector().unwrap(),
        c.store.version_vector().unwrap()
    );

    // idempotent second round
    let again = b.engine.sync_now(None, None).await;
    assert_eq!((again[0].events_received, again[0].events_sent), (0, 0));

    a.transport.shutdown().await;
    b.transport.shutdown().await;
    c.transport.shutdown().await;
}
