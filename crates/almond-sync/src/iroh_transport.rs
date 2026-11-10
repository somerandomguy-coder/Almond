//! [`Transport`] over iroh (pinned `iroh = "=1.4.1"`, `iroh-mdns-address-lookup = "=0.6.1"`).
//!
//! - **LAN-only:** built from `presets::Minimal` → no relays, no DNS/pkarr publishing; only direct
//!   UDP/QUIC paths. Peers are found via local mDNS ([`MdnsAddressLookup`], service `almond`),
//!   manual `ip:port` ([`Transport::add_addr`]), addresses injected from Android NSD
//!   ([`IrohTransport::add_discovered`]), or the last addresses remembered for a paired peer.
//! - **Two ALPNs:** `almond/sync/1` accepts only paired peers (checked against the [`PeerBook`]
//!   right after the TLS handshake proved the remote's ed25519 key); `almond/pair/1` is only
//!   served while a pairing session started by [`IrohTransport::start_pairing`] is open.
//! - **Framing:** one bidirectional QUIC stream per session, `u32-BE length + payload` frames.

use crate::identity::{DeviceIdentity, device_id, endpoint_id};
use crate::pairing::{self, PairingSecret};
use crate::peers::{PairedPeer, PeerBook};
use almond_types::sync::{Connection, DeviceInfo, PairingTicket, PeerInfo, Transport};
use almond_types::time::{UnixMs, now_ms};
use almond_types::{DeviceId, Error, Result};
use async_trait::async_trait;
use iroh::address_lookup::UserData;
use iroh::endpoint::{self, presets};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr, TransportAddr};
use iroh_mdns_address_lookup::{DiscoveryEvent, MdnsAddressLookup};
use n0_future::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};

pub const SYNC_ALPN: &[u8] = b"almond/sync/1";
pub const PAIR_ALPN: &[u8] = b"almond/pair/1";
/// mDNS service name (`<id>._almond._udp.local`). Android NSD should use [`NSD_SERVICE_TYPE`].
pub const MDNS_SERVICE: &str = "almond";
pub const NSD_SERVICE_TYPE: &str = "_almond._udp";
/// Default UDP port (fixed so manual `ip:port` entry stays valid across restarts).
pub const DEFAULT_PORT: u16 = 47321;
const MAX_PAIR_FAILURES: u32 = 5;
const FRAME_LIMIT: usize = crate::protocol::MAX_FRAME;

#[derive(Debug, Clone)]
pub struct IrohConfig {
    /// UDP port; 0 = random. If the port is taken, a random one is used instead.
    pub port: u16,
    /// Bind to this IP only (e.g. 127.0.0.1 for tests). `None` = all interfaces (v4 + v6).
    pub bind_ip: Option<IpAddr>,
    /// Local mDNS discovery + advertisement.
    pub mdns: bool,
}

impl Default for IrohConfig {
    fn default() -> Self {
        IrohConfig {
            port: DEFAULT_PORT,
            bind_ip: None,
            mdns: true,
        }
    }
}

/// What Android NSD (WS-G) should register so other devices can find this one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NsdAdvertisement {
    pub service_type: String,
    /// Instance name; use the device id so it is unique.
    pub service_name: String,
    pub port: u16,
    /// TXT records: `id` (device id hex), `n` (name), `p` (platform).
    pub txt: BTreeMap<String, String>,
}

/// Compact info published as mDNS user data (≤ 245 bytes).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Advert {
    /// app marker, always "almond"
    a: String,
    n: String,
    p: String,
    /// pairing session open
    #[serde(default)]
    o: bool,
}

#[derive(Debug, Clone)]
struct Seen {
    addrs: Vec<SocketAddr>,
    name: Option<String>,
    platform: Option<String>,
    pairing_open: bool,
    last_seen: UnixMs,
}

struct PairingSession {
    secret: PairingSecret,
    expires_at: UnixMs,
    failures: u32,
}

struct Inner {
    endpoint: Endpoint,
    me: DeviceInfo,
    book: Arc<PeerBook>,
    seen: Mutex<BTreeMap<DeviceId, Seen>>,
    incoming_tx: mpsc::Sender<Box<dyn Connection>>,
    incoming_rx: tokio::sync::Mutex<mpsc::Receiver<Box<dyn Connection>>>,
    pairing: Mutex<Option<PairingSession>>,
    paired_tx: broadcast::Sender<DeviceInfo>,
}

impl Inner {
    fn seen_lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<DeviceId, Seen>> {
        self.seen.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn note_seen(&self, id: DeviceId, addrs: Vec<SocketAddr>, adv: Option<Advert>) {
        if id == self.me.id {
            return;
        }
        let mut g = self.seen_lock();
        let e = g.entry(id).or_insert(Seen {
            addrs: vec![],
            name: None,
            platform: None,
            pairing_open: false,
            last_seen: 0,
        });
        for a in addrs {
            if !e.addrs.contains(&a) {
                e.addrs.insert(0, a);
            }
        }
        e.addrs.truncate(8);
        if let Some(a) = adv {
            e.name = Some(a.n);
            e.platform = Some(a.p);
            e.pairing_open = a.o;
        }
        e.last_seen = now_ms();
    }

    fn addrs_for(&self, d: &DeviceId) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> = self
            .seen_lock()
            .get(d)
            .map(|s| s.addrs.clone())
            .unwrap_or_default();
        if let Some(p) = self.book.get(d) {
            for a in p.addrs.iter().filter_map(|a| a.parse().ok()) {
                if !v.contains(&a) {
                    v.push(a);
                }
            }
        }
        v
    }

    fn set_advert(&self, pairing_open: bool) {
        let adv = Advert {
            a: "almond".into(),
            n: self.me.name.chars().take(64).collect(),
            p: self.me.platform.clone(),
            o: pairing_open,
        };
        let s = serde_json::to_string(&adv).unwrap_or_default();
        if let Ok(ud) = UserData::try_from(s) {
            self.endpoint.set_user_data_for_address_lookup(Some(ud));
        }
    }

    async fn dial(&self, peer: &DeviceId, alpn: &[u8]) -> Result<IrohConnection> {
        let id = endpoint_id(peer)?;
        let addrs = self.addrs_for(peer);
        let addr = EndpointAddr::from_parts(id, addrs.into_iter().map(TransportAddr::Ip));
        let conn = self
            .endpoint
            .connect(addr, alpn)
            .await
            .map_err(|e| Error::Network(format!("connect {peer}: {e}")))?;
        let working = remote_ip_addrs(&conn);
        if !working.is_empty() {
            let _ = self
                .book
                .note_addrs(peer, working.iter().map(|a| a.to_string()).collect());
        }
        let (send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| Error::Network(format!("open stream: {e}")))?;
        Ok(IrohConnection {
            peer: peer.clone(),
            conn,
            send,
            recv,
        })
    }
}

/// The production transport.
pub struct IrohTransport {
    inner: Arc<Inner>,
    router: Router,
    mdns: Option<MdnsAddressLookup>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl IrohTransport {
    /// Bind the endpoint and start accepting. `me.id` must equal `identity.device_id()`.
    pub async fn bind(
        identity: &DeviceIdentity,
        me: DeviceInfo,
        book: Arc<PeerBook>,
        cfg: IrohConfig,
    ) -> Result<Self> {
        if me.id != identity.device_id() {
            return Err(Error::Invalid(
                "DeviceInfo.id must match the identity".into(),
            ));
        }
        let ep = match Self::bind_endpoint(identity, &cfg, cfg.port).await {
            Ok(ep) => ep,
            Err(e) if cfg.port != 0 => {
                tracing::warn!("port {} unavailable ({e}); using a random port", cfg.port);
                Self::bind_endpoint(identity, &cfg, 0).await?
            }
            Err(e) => return Err(e),
        };

        let (incoming_tx, incoming_rx) = mpsc::channel(16);
        let (paired_tx, _) = broadcast::channel(8);
        let inner = Arc::new(Inner {
            endpoint: ep.clone(),
            me,
            book,
            seen: Mutex::new(BTreeMap::new()),
            incoming_tx,
            incoming_rx: tokio::sync::Mutex::new(incoming_rx),
            pairing: Mutex::new(None),
            paired_tx,
        });
        inner.set_advert(false);

        let mut tasks = vec![];
        let mdns = if cfg.mdns {
            match MdnsAddressLookup::builder()
                .service_name(MDNS_SERVICE)
                .build(ep.id())
            {
                Ok(m) => {
                    ep.address_lookup()
                        .map_err(|e| Error::Other(format!("address lookup: {e}")))?
                        .add(m.clone());
                    let mut events = m.subscribe().await;
                    let inner2 = inner.clone();
                    tasks.push(tokio::spawn(async move {
                        while let Some(ev) = events.next().await {
                            match ev {
                                DiscoveryEvent::Discovered { endpoint_info, .. } => {
                                    let adv = endpoint_info
                                        .user_data()
                                        .and_then(|u| {
                                            serde_json::from_str::<Advert>(u.as_ref()).ok()
                                        })
                                        .filter(|a| a.a == "almond");
                                    let addrs = endpoint_info.ip_addrs().copied().collect();
                                    inner2.note_seen(
                                        device_id(&endpoint_info.endpoint_id),
                                        addrs,
                                        adv,
                                    );
                                }
                                DiscoveryEvent::Expired { endpoint_id } => {
                                    inner2.seen_lock().remove(&device_id(&endpoint_id));
                                }
                                _ => {}
                            }
                        }
                    }));
                    Some(m)
                }
                Err(e) => {
                    tracing::warn!("mDNS unavailable ({e}); use manual addresses / NSD");
                    None
                }
            }
        } else {
            None
        };

        let router = Router::builder(ep)
            .accept(SYNC_ALPN, SyncHandler(inner.clone()))
            .accept(PAIR_ALPN, PairHandler(inner.clone()))
            .spawn();
        Ok(IrohTransport {
            inner,
            router,
            mdns,
            tasks,
        })
    }

    async fn bind_endpoint(id: &DeviceIdentity, cfg: &IrohConfig, port: u16) -> Result<Endpoint> {
        let mut b = Endpoint::builder(presets::Minimal)
            .secret_key(id.secret_key().clone())
            .relay_mode(iroh::RelayMode::Disabled);
        let berr = |e: endpoint::InvalidSocketAddr| Error::Invalid(format!("bind addr: {e}"));
        match cfg.bind_ip {
            Some(ip) => {
                b = b
                    .clear_ip_transports()
                    .bind_addr(SocketAddr::new(ip, port))
                    .map_err(berr)?;
            }
            None if port != 0 => {
                b = b
                    .clear_ip_transports()
                    .bind_addr(SocketAddr::new(IpAddr::from([0, 0, 0, 0]), port))
                    .map_err(berr)?
                    .bind_addr_with_opts(
                        SocketAddr::new(IpAddr::from([0u16; 8]), port),
                        endpoint::BindOpts::default().set_is_required(false),
                    )
                    .map_err(berr)?;
            }
            None => {}
        }
        b.bind()
            .await
            .map_err(|e| Error::Network(format!("bind endpoint: {e}")))
    }

    pub fn device(&self) -> &DeviceInfo {
        &self.inner.me
    }

    pub fn peer_book(&self) -> &Arc<PeerBook> {
        &self.inner.book
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.endpoint
    }

    /// Our reachable socket addresses (what goes into a pairing ticket / NSD record).
    pub fn local_addrs(&self) -> Vec<SocketAddr> {
        let mut v: Vec<SocketAddr> = self.inner.endpoint.addr().ip_addrs().copied().collect();
        if v.is_empty() {
            v = self
                .inner
                .endpoint
                .bound_sockets()
                .into_iter()
                .filter(|a| !a.ip().is_unspecified())
                .collect();
        }
        v
    }

    pub fn local_port(&self) -> Option<u16> {
        self.inner
            .endpoint
            .bound_sockets()
            .first()
            .map(|a| a.port())
    }

    /// Data for Android `NsdManager.registerService` (WS-G).
    pub fn nsd_advertisement(&self) -> NsdAdvertisement {
        let mut txt = BTreeMap::new();
        txt.insert("id".into(), self.inner.me.id.0.clone());
        txt.insert("n".into(), self.inner.me.name.chars().take(64).collect());
        txt.insert("p".into(), self.inner.me.platform.clone());
        NsdAdvertisement {
            service_type: NSD_SERVICE_TYPE.into(),
            service_name: self.inner.me.id.0.clone(),
            port: self.local_port().unwrap_or(0),
            txt,
        }
    }

    /// Hook for platform discovery (Android NSD, WS-G): report a peer seen on the LAN.
    pub fn add_discovered(&self, device: DeviceId, addrs: Vec<SocketAddr>, name: Option<String>) {
        let adv = name.map(|n| Advert {
            a: "almond".into(),
            n,
            p: String::new(),
            o: false,
        });
        let _ = self
            .inner
            .book
            .note_addrs(&device, addrs.iter().map(|a| a.to_string()).collect());
        self.inner.note_seen(device, addrs, adv);
    }

    // ---- pairing ----

    /// Open a pairing session (host side) and return the ticket to show as QR / 6 words.
    pub fn start_pairing(&self, ttl: Duration) -> PairingTicket {
        let secret = PairingSecret::generate();
        let expires_at = now_ms() + ttl.as_millis() as i64;
        let ticket = pairing::make_ticket(
            self.inner.me.clone(),
            self.local_addrs().iter().map(|a| a.to_string()).collect(),
            &secret,
            expires_at,
        );
        *self.inner.pairing.lock().unwrap_or_else(|p| p.into_inner()) = Some(PairingSession {
            secret,
            expires_at,
            failures: 0,
        });
        self.inner.set_advert(true);
        ticket
    }

    pub fn cancel_pairing(&self) {
        *self.inner.pairing.lock().unwrap_or_else(|p| p.into_inner()) = None;
        self.inner.set_advert(false);
    }

    /// Fires with the joiner's `DeviceInfo` whenever a pairing completes on the host side.
    pub fn subscribe_paired(&self) -> broadcast::Receiver<DeviceInfo> {
        self.inner.paired_tx.subscribe()
    }

    /// Joiner side, from a scanned QR ticket.
    pub async fn pair_with_ticket(&self, t: &PairingTicket) -> Result<DeviceInfo> {
        if pairing::ticket_expired(t) {
            return Err(Error::Invalid("pairing code expired".into()));
        }
        let secret = PairingSecret::from_base32(&t.secret)?;
        let addrs: Vec<SocketAddr> = t.addrs.iter().filter_map(|a| a.parse().ok()).collect();
        self.inner.note_seen(t.device.id.clone(), addrs, None);
        self.pair_with(&t.device.id, &secret).await
    }

    /// Joiner side, from 6 typed words: try every discovered Almond device with an open
    /// pairing session (each wrong guess costs the host one of its few allowed failures, so we
    /// only try devices that advertise an open session, unless `host` is given).
    pub async fn pair_with_code(&self, words: &str, host: Option<DeviceId>) -> Result<DeviceInfo> {
        let secret = PairingSecret::from_words(words)?;
        let candidates: Vec<DeviceId> = match host {
            Some(h) => vec![h],
            None => self
                .inner
                .seen_lock()
                .iter()
                .filter(|(id, s)| s.pairing_open && !self.inner.book.is_paired(id))
                .map(|(id, _)| id.clone())
                .collect(),
        };
        if candidates.is_empty() {
            return Err(Error::NotFound(
                "no device with an open pairing session found on the LAN".into(),
            ));
        }
        let mut last = Error::NotFound("no candidates".into());
        for c in candidates {
            match self.pair_with(&c, &secret).await {
                Ok(info) => return Ok(info),
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    async fn pair_with(&self, host: &DeviceId, secret: &PairingSecret) -> Result<DeviceInfo> {
        let mut c = tokio::time::timeout(Duration::from_secs(15), self.inner.dial(host, PAIR_ALPN))
            .await
            .map_err(|_| Error::Network("pairing connect timed out".into()))??;
        let r = tokio::time::timeout(
            Duration::from_secs(30),
            pairing::handshake_initiator(&mut c, secret, &self.inner.me),
        )
        .await
        .map_err(|_| Error::Network("pairing timed out".into()))?;
        let _ = c.close().await;
        let info = r?;
        let addrs = self
            .inner
            .addrs_for(host)
            .iter()
            .map(|a| a.to_string())
            .collect();
        self.inner.book.add(info.clone(), addrs)?;
        Ok(info)
    }

    pub fn paired_peers(&self) -> Vec<PairedPeer> {
        self.inner.book.list()
    }

    pub fn unpair(&self, d: &DeviceId) -> Result<bool> {
        self.inner.book.remove(d)
    }

    /// Graceful shutdown (closes connections, stops mDNS).
    pub async fn shutdown(&self) {
        for t in &self.tasks {
            t.abort();
        }
        let _ = self.router.shutdown().await;
        drop(self.mdns.clone());
    }
}

#[async_trait]
impl Transport for IrohTransport {
    fn local_device(&self) -> DeviceId {
        self.inner.me.id.clone()
    }

    async fn discover(&self) -> Result<Vec<PeerInfo>> {
        let seen = self.inner.seen_lock().clone();
        let mut out: BTreeMap<DeviceId, PeerInfo> = BTreeMap::new();
        for p in self.inner.book.list() {
            out.insert(
                p.info.id.clone(),
                PeerInfo {
                    device: p.info.clone(),
                    addrs: p.addrs.clone(),
                    paired: true,
                    last_seen: p.last_seen,
                    last_synced: p.last_synced,
                },
            );
        }
        for (id, s) in seen {
            let addrs: Vec<String> = s.addrs.iter().map(|a| a.to_string()).collect();
            match out.get_mut(&id) {
                Some(p) => {
                    for a in addrs {
                        if !p.addrs.contains(&a) {
                            p.addrs.insert(0, a);
                        }
                    }
                    p.last_seen = Some(s.last_seen.max(p.last_seen.unwrap_or(0)));
                }
                None => {
                    out.insert(
                        id.clone(),
                        PeerInfo {
                            device: DeviceInfo {
                                id,
                                name: s.name.unwrap_or_else(|| "Unknown device".into()),
                                platform: s.platform.unwrap_or_default(),
                                app_version: String::new(),
                                capabilities: vec![],
                                last_active: None,
                            },
                            addrs,
                            paired: false,
                            last_seen: Some(s.last_seen),
                            last_synced: None,
                        },
                    );
                }
            }
        }
        Ok(out.into_values().collect())
    }

    /// Manual `ip:port`. iroh authenticates peers by key, so the device id must be known
    /// (from the pairing QR, NSD TXT record, or the paired-devices list).
    async fn add_addr(&self, d: DeviceId, addr: String) -> Result<()> {
        let sa: SocketAddr = addr
            .trim()
            .parse()
            .map_err(|e| Error::Invalid(format!("address {addr:?}: {e}")))?;
        endpoint_id(&d)?;
        self.inner.book.note_addrs(&d, vec![sa.to_string()])?;
        self.inner.note_seen(d, vec![sa], None);
        Ok(())
    }

    async fn connect(&self, peer: &DeviceId) -> Result<Box<dyn Connection>> {
        if !self.inner.book.is_paired(peer) {
            return Err(Error::PermissionDenied(format!("{peer} is not paired")));
        }
        Ok(Box::new(self.inner.dial(peer, SYNC_ALPN).await?))
    }

    async fn accept(&self) -> Result<Box<dyn Connection>> {
        self.inner
            .incoming_rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| Error::Network("transport shut down".into()))
    }
}

// ------------------------------------------------------------------ handlers

/// Direct IP addresses the remote is reachable at, from the live connection's paths. iroh sends
/// from its bound socket, so on a LAN the source address is the peer's listening address; this
/// is how the *host* side of a pairing (and every later incoming sync) learns where to dial
/// back, even without mDNS.
fn remote_ip_addrs(conn: &endpoint::Connection) -> Vec<SocketAddr> {
    let paths = conn.paths();
    let mut v: Vec<SocketAddr> = vec![];
    for p in paths.iter() {
        if let TransportAddr::Ip(a) = p.remote_addr()
            && !v.contains(a)
        {
            if p.is_selected() {
                v.insert(0, *a);
            } else {
                v.push(*a);
            }
        }
    }
    v
}

struct SyncHandler(Arc<Inner>);
struct PairHandler(Arc<Inner>);

impl std::fmt::Debug for SyncHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SyncHandler")
    }
}
impl std::fmt::Debug for PairHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairHandler")
    }
}

impl ProtocolHandler for SyncHandler {
    async fn accept(&self, conn: endpoint::Connection) -> std::result::Result<(), AcceptError> {
        let peer = device_id(&conn.remote_id());
        if !self.0.book.is_paired(&peer) {
            tracing::info!("rejecting sync from unpaired device {peer}");
            conn.close(403u32.into(), b"not paired");
            return Ok(());
        }
        let addrs = remote_ip_addrs(&conn);
        if !addrs.is_empty() {
            let _ = self
                .0
                .book
                .note_addrs(&peer, addrs.iter().map(|a| a.to_string()).collect());
            self.0.note_seen(peer.clone(), addrs, None);
        }
        let (send, recv) = conn.accept_bi().await?;
        let c = IrohConnection {
            peer,
            conn,
            send,
            recv,
        };
        if self.0.incoming_tx.send(Box::new(c)).await.is_err() {
            tracing::warn!("sync connection dropped: nobody is accepting");
        }
        Ok(())
    }
}

impl ProtocolHandler for PairHandler {
    async fn accept(&self, conn: endpoint::Connection) -> std::result::Result<(), AcceptError> {
        let inner = &self.0;
        let peer = device_id(&conn.remote_id());
        let secret = {
            let mut g = inner.pairing.lock().unwrap_or_else(|p| p.into_inner());
            match g.as_ref() {
                Some(s) if s.expires_at > now_ms() => Some(s.secret.clone()),
                Some(_) => {
                    *g = None;
                    None
                }
                None => None,
            }
        };
        let Some(secret) = secret else {
            conn.close(403u32.into(), b"no pairing session");
            return Ok(());
        };
        inner.note_seen(peer.clone(), remote_ip_addrs(&conn), None);
        let (send, recv) = conn.accept_bi().await?;
        let mut c = IrohConnection {
            peer: peer.clone(),
            conn,
            send,
            recv,
        };
        let r = tokio::time::timeout(
            Duration::from_secs(30),
            pairing::handshake_responder(&mut c, &secret, &inner.me),
        )
        .await
        .unwrap_or_else(|_| Err(Error::Network("pairing timed out".into())));
        let _ = c.close().await;
        match r {
            Ok(info) => {
                let addrs = inner
                    .addrs_for(&peer)
                    .iter()
                    .map(|a| a.to_string())
                    .collect();
                if let Err(e) = inner.book.add(info.clone(), addrs) {
                    tracing::warn!("saving paired peer failed: {e}");
                    return Ok(());
                }
                *inner.pairing.lock().unwrap_or_else(|p| p.into_inner()) = None; // one-time
                inner.set_advert(false);
                let _ = inner.paired_tx.send(info);
            }
            Err(e) => {
                tracing::info!("pairing attempt from {peer} failed: {e}");
                let mut g = inner.pairing.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(s) = g.as_mut() {
                    s.failures += 1;
                    if s.failures >= MAX_PAIR_FAILURES {
                        *g = None;
                        drop(g);
                        inner.set_advert(false);
                    }
                }
            }
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ connection

pub struct IrohConnection {
    peer: DeviceId,
    conn: endpoint::Connection,
    send: endpoint::SendStream,
    recv: endpoint::RecvStream,
}

#[async_trait]
impl Connection for IrohConnection {
    fn peer(&self) -> &DeviceId {
        &self.peer
    }

    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        if msg.len() > FRAME_LIMIT {
            return Err(Error::Invalid("frame too large".into()));
        }
        self.send
            .write_all(&(msg.len() as u32).to_be_bytes())
            .await
            .map_err(|e| Error::Network(format!("send: {e}")))?;
        self.send
            .write_all(&msg)
            .await
            .map_err(|e| Error::Network(format!("send: {e}")))
    }

    async fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        let mut len = [0u8; 4];
        match self.recv.read_exact(&mut len).await {
            Ok(()) => {}
            Err(endpoint::ReadExactError::FinishedEarly(0)) => return Ok(None),
            Err(e) => {
                // a peer that closed the whole connection after its last frame = clean close
                if self.conn.close_reason().is_some() {
                    return Ok(None);
                }
                return Err(Error::Network(format!("recv: {e}")));
            }
        }
        let n = u32::from_be_bytes(len) as usize;
        if n > FRAME_LIMIT {
            return Err(Error::Invalid("frame too large".into()));
        }
        let mut buf = vec![0u8; n];
        self.recv
            .read_exact(&mut buf)
            .await
            .map_err(|e| Error::Network(format!("recv: {e}")))?;
        Ok(Some(buf))
    }

    /// Finish our side, give the peer a moment to finish theirs, then close.
    async fn close(&mut self) -> Result<()> {
        let _ = self.send.finish();
        let _ = tokio::time::timeout(Duration::from_secs(3), async {
            let mut buf = [0u8; 4096];
            while let Ok(Some(_)) = self.recv.read(&mut buf).await {}
        })
        .await;
        self.conn.close(0u32.into(), b"bye");
        Ok(())
    }
}
