//! Pairing: tickets (QR payload + 6-word code) and the mutual-authentication handshake.
//!
//! The host (device showing the QR / code) generates a one-time 48-bit secret. The joiner gets it
//! either from the QR string (which also carries the host's device id and LAN addresses) or as
//! six words typed in by the user (host then found via mDNS / NSD). Both run SPAKE2 with that
//! secret over a transport-authenticated channel (iroh: TLS with raw ed25519 keys, so each side
//! already knows the *other's* device id for certain), then exchange key-confirmation MACs bound
//! to both device ids and both `DeviceInfo`s. A passive or active attacker who doesn't know the
//! secret learns nothing it can brute-force offline (that's what the PAKE buys over a plain
//! challenge/response with a short code), and gets one online guess per attempt; the host
//! closes the session after a few failures.

use crate::wordlist;
use almond_types::sync::{Connection, DeviceInfo, PairingTicket};
use almond_types::time::{UnixMs, now_ms};
use almond_types::{DeviceId, Error, Result};
use data_encoding::{BASE32_NOPAD, HEXLOWER};
use serde::{Deserialize, Serialize};
use spake2::{Ed25519Group, Identity, Password, Spake2};

pub const SECRET_LEN: usize = 6;
const QR_PREFIX: &str = "ALMOND1:";
const SPAKE_ID: &[u8] = b"almond-pair-v1";

/// One-time pairing secret (6 bytes = 6 words).
#[derive(Clone, PartialEq, Eq)]
pub struct PairingSecret(pub [u8; SECRET_LEN]);

impl std::fmt::Debug for PairingSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PairingSecret(***)")
    }
}

impl PairingSecret {
    pub fn generate() -> Self {
        let mut b = [0u8; SECRET_LEN];
        getrandom::fill(&mut b).expect("OS RNG");
        PairingSecret(b)
    }

    /// e.g. "apple-river-mint-oak-delta-fox"
    pub fn to_words(&self) -> String {
        self.0
            .iter()
            .map(|b| wordlist::WORDS[*b as usize])
            .collect::<Vec<_>>()
            .join("-")
    }

    /// Accepts words separated by spaces, hyphens or commas; case-insensitive.
    pub fn from_words(s: &str) -> Result<Self> {
        let words: Vec<&str> = s
            .split(|c: char| c.is_whitespace() || c == '-' || c == ',')
            .filter(|w| !w.is_empty())
            .collect();
        if words.len() != SECRET_LEN {
            return Err(Error::Invalid(format!(
                "pairing code needs {SECRET_LEN} words, got {}",
                words.len()
            )));
        }
        let mut b = [0u8; SECRET_LEN];
        for (i, w) in words.iter().enumerate() {
            b[i] = wordlist::index_of(w)
                .ok_or_else(|| Error::Invalid(format!("unknown pairing word {w:?}")))?;
        }
        Ok(PairingSecret(b))
    }

    pub fn to_base32(&self) -> String {
        BASE32_NOPAD.encode(&self.0)
    }

    pub fn from_base32(s: &str) -> Result<Self> {
        let v = BASE32_NOPAD
            .decode(s.trim().to_ascii_uppercase().as_bytes())
            .map_err(|e| Error::Invalid(format!("pairing secret: {e}")))?;
        let arr: [u8; SECRET_LEN] = v
            .try_into()
            .map_err(|_| Error::Invalid("pairing secret has wrong length".into()))?;
        Ok(PairingSecret(arr))
    }
}

pub fn make_ticket(
    device: DeviceInfo,
    addrs: Vec<String>,
    secret: &PairingSecret,
    expires_at: UnixMs,
) -> PairingTicket {
    PairingTicket {
        device,
        addrs,
        secret: secret.to_base32(),
        expires_at,
    }
}

/// Encode a ticket as a QR-friendly string (uppercase base32 → QR alphanumeric mode).
pub fn ticket_to_qr(t: &PairingTicket) -> String {
    let json = serde_json::to_vec(t).expect("ticket serializes");
    format!("{QR_PREFIX}{}", BASE32_NOPAD.encode(&json))
}

pub fn ticket_from_qr(s: &str) -> Result<PairingTicket> {
    let s = s.trim();
    let body = s
        .strip_prefix(QR_PREFIX)
        .ok_or_else(|| Error::Invalid("not an Almond pairing code".into()))?;
    let json = BASE32_NOPAD
        .decode(body.as_bytes())
        .map_err(|e| Error::Invalid(format!("pairing QR: {e}")))?;
    let t: PairingTicket = serde_json::from_slice(&json)?;
    PairingSecret::from_base32(&t.secret)?;
    Ok(t)
}

pub fn ticket_expired(t: &PairingTicket) -> bool {
    now_ms() > t.expires_at
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
enum PairMsg {
    Start { device: DeviceInfo, spake: String },
    Confirm { mac: String },
    Done,
    Fail { reason: String },
}

async fn send(conn: &mut dyn Connection, m: &PairMsg) -> Result<()> {
    conn.send(serde_json::to_vec(m)?).await
}

async fn recv(conn: &mut dyn Connection) -> Result<PairMsg> {
    let b = conn
        .recv()
        .await?
        .ok_or_else(|| Error::Network("peer closed during pairing".into()))?;
    if b.len() > 64 * 1024 {
        return Err(Error::Invalid("pairing message too large".into()));
    }
    match serde_json::from_slice(&b)? {
        PairMsg::Fail { reason } => Err(Error::PermissionDenied(format!(
            "pairing refused: {reason}"
        ))),
        m => Ok(m),
    }
}

struct Transcript {
    key: [u8; 32],
    th: [u8; 32],
}

impl Transcript {
    fn new(
        spake_key: &[u8],
        init_id: &DeviceId,
        resp_id: &DeviceId,
        init_msg: &[u8],
        resp_msg: &[u8],
        init_info: &DeviceInfo,
        resp_info: &DeviceInfo,
    ) -> Result<Self> {
        let mut h = blake3::Hasher::new_derive_key("almond pair v1 transcript");
        for part in [
            init_id.0.as_bytes(),
            resp_id.0.as_bytes(),
            init_msg,
            resp_msg,
            &serde_json::to_vec(init_info)?,
            &serde_json::to_vec(resp_info)?,
        ] {
            h.update(&(part.len() as u64).to_le_bytes());
            h.update(part);
        }
        Ok(Transcript {
            key: blake3::derive_key("almond pair v1 confirm", spake_key),
            th: *h.finalize().as_bytes(),
        })
    }

    fn mac(&self, role: &[u8]) -> blake3::Hash {
        let mut h = blake3::Hasher::new_keyed(&self.key);
        h.update(role);
        h.update(&self.th);
        h.finalize()
    }

    fn check(&self, role: &[u8], mac_hex: &str) -> Result<()> {
        let got = HEXLOWER
            .decode(mac_hex.as_bytes())
            .ok()
            .and_then(|v| <[u8; 32]>::try_from(v).ok())
            .map(blake3::Hash::from_bytes);
        // blake3::Hash equality is constant-time
        if got == Some(self.mac(role)) {
            Ok(())
        } else {
            Err(Error::PermissionDenied("wrong pairing code".into()))
        }
    }
}

fn check_peer(conn: &dyn Connection, info: &DeviceInfo) -> Result<()> {
    if &info.id != conn.peer() {
        return Err(Error::PermissionDenied(format!(
            "peer claims to be {} but authenticated as {}",
            info.id,
            conn.peer()
        )));
    }
    Ok(())
}

/// Joiner side. `conn.peer()` must be the transport-authenticated id of the host.
/// Returns the host's `DeviceInfo` once both sides proved knowledge of the secret.
pub async fn handshake_initiator(
    conn: &mut dyn Connection,
    secret: &PairingSecret,
    me: &DeviceInfo,
) -> Result<DeviceInfo> {
    let (st, my_msg) =
        Spake2::<Ed25519Group>::start_symmetric(&Password::new(secret.0), &Identity::new(SPAKE_ID));
    send(
        conn,
        &PairMsg::Start {
            device: me.clone(),
            spake: HEXLOWER.encode(&my_msg),
        },
    )
    .await?;
    let PairMsg::Start {
        device: host,
        spake,
    } = recv(conn).await?
    else {
        return Err(Error::Invalid("unexpected pairing message".into()));
    };
    check_peer(conn, &host)?;
    let their = HEXLOWER
        .decode(spake.as_bytes())
        .map_err(|e| Error::Invalid(format!("spake msg: {e}")))?;
    let key = st
        .finish(&their)
        .map_err(|e| Error::Invalid(format!("spake: {e:?}")))?;
    let t = Transcript::new(&key, &me.id, &host.id, &my_msg, &their, me, &host)?;
    send(
        conn,
        &PairMsg::Confirm {
            mac: t.mac(b"initiator").to_hex().to_string(),
        },
    )
    .await?;
    let PairMsg::Confirm { mac } = recv(conn).await? else {
        return Err(Error::Invalid("unexpected pairing message".into()));
    };
    t.check(b"responder", &mac)?;
    send(conn, &PairMsg::Done).await?;
    Ok(host)
}

/// Host side. Returns the joiner's `DeviceInfo` after mutual confirmation (incl. the final
/// `Done`, so both sides commit the pairing).
pub async fn handshake_responder(
    conn: &mut dyn Connection,
    secret: &PairingSecret,
    me: &DeviceInfo,
) -> Result<DeviceInfo> {
    let PairMsg::Start {
        device: joiner,
        spake,
    } = recv(conn).await?
    else {
        return Err(Error::Invalid("unexpected pairing message".into()));
    };
    check_peer(conn, &joiner)?;
    let their = HEXLOWER
        .decode(spake.as_bytes())
        .map_err(|e| Error::Invalid(format!("spake msg: {e}")))?;
    let (st, my_msg) =
        Spake2::<Ed25519Group>::start_symmetric(&Password::new(secret.0), &Identity::new(SPAKE_ID));
    send(
        conn,
        &PairMsg::Start {
            device: me.clone(),
            spake: HEXLOWER.encode(&my_msg),
        },
    )
    .await?;
    let key = st
        .finish(&their)
        .map_err(|e| Error::Invalid(format!("spake: {e:?}")))?;
    let t = Transcript::new(&key, &joiner.id, &me.id, &their, &my_msg, &joiner, me)?;
    let PairMsg::Confirm { mac } = recv(conn).await? else {
        return Err(Error::Invalid("unexpected pairing message".into()));
    };
    if let Err(e) = t.check(b"initiator", &mac) {
        let _ = send(
            conn,
            &PairMsg::Fail {
                reason: "wrong code".into(),
            },
        )
        .await;
        return Err(e);
    }
    send(
        conn,
        &PairMsg::Confirm {
            mac: t.mac(b"responder").to_hex().to_string(),
        },
    )
    .await?;
    match recv(conn).await? {
        PairMsg::Done => Ok(joiner),
        _ => Err(Error::Invalid("unexpected pairing message".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use almond_types::mock::{MemNetwork, fixtures};
    use almond_types::sync::Transport;

    #[test]
    fn words_and_qr_roundtrip() {
        let s = PairingSecret::generate();
        let w = s.to_words();
        assert_eq!(w.split('-').count(), 6);
        assert_eq!(
            PairingSecret::from_words(&w.replace('-', "  ").to_uppercase()).unwrap(),
            s
        );
        assert!(PairingSecret::from_words("apple river").is_err());
        assert!(PairingSecret::from_words("apple river mint oak delta zzzz").is_err());
        let t = make_ticket(
            fixtures::device("pc"),
            vec!["192.168.1.2:47321".into()],
            &s,
            now_ms() + 60_000,
        );
        let qr = ticket_to_qr(&t);
        assert!(
            qr.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ':')
        );
        assert_eq!(ticket_from_qr(&qr).unwrap(), t);
        assert!(!ticket_expired(&t));
        assert!(ticket_from_qr("hello").is_err());
    }

    async fn run(
        host_secret: PairingSecret,
        join_secret: PairingSecret,
    ) -> (Result<DeviceInfo>, Result<DeviceInfo>) {
        let net = MemNetwork::new();
        let host = fixtures::device("host");
        let joiner = fixtures::device("joiner");
        let th = net.transport(host.clone());
        let tj = net.transport(joiner.clone());
        let mut cj = tj.connect(&host.id).await.unwrap();
        let mut ch = th.accept().await.unwrap();
        let h = tokio::spawn(
            async move { handshake_responder(ch.as_mut(), &host_secret, &host).await },
        );
        let j = handshake_initiator(cj.as_mut(), &join_secret, &joiner).await;
        drop(cj);
        (j, h.await.unwrap())
    }

    #[tokio::test]
    async fn handshake_succeeds_with_same_secret() {
        let s = PairingSecret::generate();
        let (j, h) = run(s.clone(), s).await;
        assert_eq!(j.unwrap().name, "host");
        assert_eq!(h.unwrap().name, "joiner");
    }

    #[tokio::test]
    async fn handshake_fails_with_wrong_secret() {
        let (j, h) = run(PairingSecret([1; 6]), PairingSecret([2; 6])).await;
        assert!(j.is_err());
        assert!(h.is_err());
    }
}
