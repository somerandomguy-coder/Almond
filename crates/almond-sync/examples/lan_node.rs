//! A scriptable Almond sync node, one per process. Used by `tests/multiprocess.rs` and handy for
//! manual LAN testing between real machines:
//!
//! ```text
//! cargo run -p almond-sync --example lan_node -- --dir /tmp/a --name PC [--port 47321] [--bind 0.0.0.0] [--no-mdns]
//! ```
//!
//! Then type commands (one per line; replies are single lines starting with `OK` or `ERR`):
//! `id`, `addrs`, `ticket` (prints QR string and 6 words), `pair <qr>`,
//! `paircode <host-id> <host-addr|-> <word> x6`, `peers`, `discover`, `add <title>`,
//! `blob <bytes> <mime>`, `sync`, `dump`, `quit`.

use almond_store::FsBlobStore;
use almond_sync::secrets::FileSecretStore;
use almond_sync::{IrohConfig, NodeConfig, SyncNode, ticket_from_qr, ticket_to_qr};
use almond_types::event::EventBody;
use almond_types::mock::fixtures;
use almond_types::store::{BlobStore, MeetingQuery, Store};
use almond_types::sync::Transport;
use almond_types::{DeviceId, Result};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = arg(&args, "--dir").expect("--dir required");
    let name = arg(&args, "--name").unwrap_or_else(|| "node".into());
    let mut cfg = NodeConfig::new(&dir, &name);
    // per-node file secrets so several nodes on one machine don't share the OS keychain entry
    cfg.secrets = Some(Arc::new(FileSecretStore::new(
        std::path::Path::new(&dir).join("secrets.json"),
    )));
    cfg.iroh = IrohConfig {
        port: arg(&args, "--port")
            .map(|p| p.parse().unwrap())
            .unwrap_or(0),
        bind_ip: arg(&args, "--bind").map(|b| b.parse::<IpAddr>().unwrap()),
        mdns: !args.iter().any(|a| a == "--no-mdns"),
    };
    let node = match SyncNode::open(cfg).await {
        Ok(n) => n,
        Err(e) => {
            println!("ERR open: {e}");
            return;
        }
    };
    println!("READY {}", node.device.id);

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }
        if parts[0] == "quit" {
            break;
        }
        match handle(&node, &parts).await {
            Ok(s) => println!("OK {s}"),
            Err(e) => println!("ERR {e}"),
        }
    }
    node.shutdown().await;
}

async fn handle(node: &SyncNode, p: &[&str]) -> Result<String> {
    let t = &node.transport;
    Ok(match p[0] {
        "id" => node.device.id.0.clone(),
        "addrs" => serde_json::to_string(&t.local_addrs())?,
        "ticket" => {
            let tk = t.start_pairing(Duration::from_secs(300));
            let words = almond_sync::PairingSecret::from_base32(&tk.secret)?.to_words();
            format!("{} {}", ticket_to_qr(&tk), words)
        }
        "pair" => {
            let tk = ticket_from_qr(p.get(1).copied().unwrap_or_default())?;
            t.pair_with_ticket(&tk).await?.name
        }
        "paircode" => {
            let host = DeviceId(p[1].to_string());
            if p[2] != "-" {
                t.add_addr(host.clone(), p[2].to_string()).await?;
            }
            t.pair_with_code(&p[3..].join(" "), Some(host)).await?.name
        }
        "peers" => serde_json::to_string(&t.paired_peers())?,
        "discover" => serde_json::to_string(&t.discover().await?)?,
        "add" => {
            let mut m = fixtures::meeting(almond_types::time::now_ms());
            m.title = p[1..].join(" ");
            node.store
                .append_local(EventBody::MeetingUpsert(m.clone()))?;
            m.id.0
        }
        "blob" => {
            let n: usize = p[1].parse().map_err(almond_types::Error::other)?;
            let data: Vec<u8> = (0..n)
                .map(|i| (i * 31 % 251) as u8)
                .chain(node.device.id.0.bytes())
                .collect();
            let h = node.blobs.put(&data)?;
            node.store.append_local(EventBody::BlobAvailable {
                hash: h.clone(),
                size: data.len() as u64,
                mime: p.get(2).unwrap_or(&"audio/ogg").to_string(),
            })?;
            h.0
        }
        "sync" => serde_json::to_string(&node.engine.sync_now(None, None).await)?,
        "dump" => dump(node, &node.blobs)?,
        other => {
            return Err(almond_types::Error::Invalid(format!(
                "unknown command {other}"
            )));
        }
    })
}

fn dump(node: &SyncNode, blobs: &FsBlobStore) -> Result<String> {
    let mut titles: Vec<String> = node
        .store
        .list_meetings(&MeetingQuery::default())?
        .into_iter()
        .map(|m| m.title)
        .collect();
    titles.sort();
    let mut have = 0;
    let known = node.store.known_blobs()?;
    for b in &known {
        if blobs.has(&b.hash)? {
            have += 1;
        }
    }
    Ok(serde_json::json!({
        "vv": node.store.version_vector()?,
        "meetings": titles,
        "devices": node.store.devices()?.len(),
        "blobs_known": known.len(),
        "blobs_have": have,
    })
    .to_string())
}
