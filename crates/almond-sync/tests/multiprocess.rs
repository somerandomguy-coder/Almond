//! 3 separate OS processes (`examples/lan_node.rs`) talking real iroh over 127.0.0.1:
//! pair (QR + 6-word code), offline edits, sync with relaying, restart with persisted identity.
//!
//! Needs the example binary, so it's ignored by default:
//! `cargo test -p almond-sync --test multiprocess -- --ignored --nocapture`

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct Proc {
    child: Child,
    stdin: ChildStdin,
    out: Lines<BufReader<ChildStdout>>,
    id: String,
}

fn example_bin() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let status = std::process::Command::new(env!("CARGO"))
        .args(["build", "-p", "almond-sync", "--example", "lan_node"])
        .current_dir(&manifest)
        .status()
        .expect("cargo build");
    assert!(status.success());
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../../target"));
    target
        .join("debug/examples")
        .join(format!("lan_node{}", std::env::consts::EXE_SUFFIX))
}

impl Proc {
    async fn start(bin: &PathBuf, dir: &std::path::Path, name: &str, port: u16) -> Proc {
        let mut child = Command::new(bin)
            .args([
                "--dir",
                dir.join(name).to_str().unwrap(),
                "--name",
                name,
                "--port",
                &port.to_string(),
                "--bind",
                "127.0.0.1",
                "--no-mdns",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn lan_node");
        let stdin = child.stdin.take().unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
        let ready = tokio::time::timeout(Duration::from_secs(30), out.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let id = ready.strip_prefix("READY ").expect(&ready).to_string();
        Proc {
            child,
            stdin,
            out,
            id,
        }
    }

    async fn cmd(&mut self, c: &str) -> String {
        self.stdin
            .write_all(format!("{c}\n").as_bytes())
            .await
            .unwrap();
        let line = tokio::time::timeout(Duration::from_secs(60), self.out.next_line())
            .await
            .expect("reply timeout")
            .unwrap()
            .unwrap();
        println!("[{}] {c:.40} -> {line:.200}", &self.id[..8]);
        line.strip_prefix("OK ")
            .unwrap_or_else(|| panic!("{c}: {line}"))
            .to_string()
    }

    async fn quit(mut self) {
        let _ = self.stdin.write_all(b"quit\n").await;
        let _ = tokio::time::timeout(Duration::from_secs(10), self.child.wait()).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "spawns example processes; run with --ignored"]
async fn three_processes_pair_and_converge() {
    let bin = example_bin();
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb, pc) = (47411u16, 47412u16, 47413u16);
    let mut a = Proc::start(&bin, dir.path(), "pc", pa).await;
    let mut b = Proc::start(&bin, dir.path(), "laptop", pb).await;
    let mut c = Proc::start(&bin, dir.path(), "phone", pc).await;

    // B pairs with A via QR string
    let t = a.cmd("ticket").await;
    let qr = t.split_whitespace().next().unwrap().to_string();
    assert_eq!(b.cmd(&format!("pair {qr}")).await, "pc");
    // C pairs with A via 6 words + manual address
    let t = a.cmd("ticket").await;
    let words: Vec<&str> = t.split_whitespace().skip(1).collect();
    let words = words.join(" ").replace('-', " ");
    assert_eq!(
        c.cmd(&format!("paircode {} 127.0.0.1:{pa} {words}", a.id))
            .await,
        "pc"
    );

    // offline edits
    a.cmd("add from pc").await;
    b.cmd("add from laptop").await;
    c.cmd("add from phone").await;
    b.cmd("blob 400000 audio/ogg").await;
    c.cmd("blob 1000 text/markdown").await;

    b.cmd("sync").await;
    c.cmd("sync").await;
    let reports = a.cmd("sync").await;
    assert!(!reports.contains("\"error\":\""), "{reports}");

    let da = a.cmd("dump").await;
    let db = b.cmd("dump").await;
    let dc = c.cmd("dump").await;
    assert_eq!(da, db);
    assert_eq!(da, dc);
    let v: serde_json::Value = serde_json::from_str(&da).unwrap();
    assert_eq!(v["meetings"].as_array().unwrap().len(), 3);
    assert_eq!(v["devices"], 3);
    assert_eq!(v["blobs_have"], 2);

    // restart A: same identity, pairings persisted, sync still works on fixed ports
    let old_id = a.id.clone();
    a.quit().await;
    let mut a = Proc::start(&bin, dir.path(), "pc", pa).await;
    assert_eq!(a.id, old_id);
    b.cmd("add after restart").await;
    let r = a.cmd("sync").await;
    assert!(!r.contains("\"error\":\""), "{r}");
    let r = c.cmd("sync").await;
    assert!(!r.contains("\"error\":\""), "{r}");
    let dc: serde_json::Value = serde_json::from_str(&c.cmd("dump").await).unwrap();
    assert_eq!(dc["meetings"].as_array().unwrap().len(), 4);

    a.quit().await;
    b.quit().await;
    c.quit().await;
}
