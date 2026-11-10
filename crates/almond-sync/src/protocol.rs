//! Wire format of a sync session (one message per transport frame).
//!
//! ```text
//! frame  = 0x00 JSON(Msg)
//!        | 0x01 u16-BE header_len JSON(ChunkHeader) raw-bytes      (blob data, no base64)
//! ```
//!
//! Session (strictly request/response, so QUIC flow control can never deadlock both sides):
//! ```text
//! I → R  Hello{vv}            R → I  Hello{vv}
//! I drives:   Pull{have} → Events{…}  (repeat until empty)
//!             BlobGet{hash, offset} → Chunk | BlobMissing   (repeat per wanted blob)
//! I → R  Turn
//! R drives the same way, then R → I  Turn
//! I → R  Bye
//! ```
//! Pulls carry a cursor (`have` = receiver's version vector advanced past everything already
//! received in this session), so peers holding non-contiguous events can't loop forever, and an
//! interrupted session resumes from the receiver's persisted version vector next time.

use almond_types::event::Event;
use almond_types::sync::{DeviceInfo, VersionVector};
use almond_types::{BlobHash, Error, Result};
use serde::{Deserialize, Serialize};

pub const PROTO_VERSION: u32 = 1;
/// Refuse frames larger than this (a single huge transcript is still far below).
pub const MAX_FRAME: usize = 64 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    Hello {
        proto: u32,
        device: DeviceInfo,
        vv: VersionVector,
    },
    Pull {
        have: VersionVector,
        limit: u32,
    },
    Events {
        events: Vec<Event>,
    },
    BlobGet {
        hash: BlobHash,
        offset: u64,
        max: u32,
    },
    BlobMissing {
        hash: BlobHash,
    },
    Turn,
    Bye,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkHeader {
    pub hash: BlobHash,
    pub offset: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    Msg(Msg),
    Chunk(ChunkHeader, Vec<u8>),
}

pub fn encode(f: &Frame) -> Result<Vec<u8>> {
    Ok(match f {
        Frame::Msg(m) => {
            let mut v = vec![0u8];
            serde_json::to_writer(&mut v, m)?;
            v
        }
        Frame::Chunk(h, data) => {
            let hdr = serde_json::to_vec(h)?;
            let mut v = Vec::with_capacity(3 + hdr.len() + data.len());
            v.push(1u8);
            v.extend_from_slice(&(hdr.len() as u16).to_be_bytes());
            v.extend_from_slice(&hdr);
            v.extend_from_slice(data);
            v
        }
    })
}

pub fn decode(b: &[u8]) -> Result<Frame> {
    match b.first() {
        Some(0) => Ok(Frame::Msg(serde_json::from_slice(&b[1..])?)),
        Some(1) if b.len() >= 3 => {
            let n = u16::from_be_bytes([b[1], b[2]]) as usize;
            if b.len() < 3 + n {
                return Err(Error::Invalid("truncated chunk header".into()));
            }
            let h: ChunkHeader = serde_json::from_slice(&b[3..3 + n])?;
            Ok(Frame::Chunk(h, b[3 + n..].to_vec()))
        }
        _ => Err(Error::Invalid("unknown sync frame".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_roundtrip() {
        let m = Frame::Msg(Msg::Pull {
            have: VersionVector::default(),
            limit: 10,
        });
        assert_eq!(decode(&encode(&m).unwrap()).unwrap(), m);
        let c = Frame::Chunk(
            ChunkHeader {
                hash: BlobHash("ab".repeat(32)),
                offset: 7,
                total: 9,
            },
            vec![0, 1, 2, 255],
        );
        assert_eq!(decode(&encode(&c).unwrap()).unwrap(), c);
        assert!(decode(&[9, 9]).is_err());
        assert!(decode(&[1, 0, 50, 1]).is_err());
    }
}
