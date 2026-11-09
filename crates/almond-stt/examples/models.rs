//! Model manager CLI.
//!
//! ```text
//! cargo run -p almond-stt --example models               # list registry + install state
//! cargo run -p almond-stt --example models -- get ID...  # download + verify + extract
//! cargo run -p almond-stt --example models -- routes     # default routing per device class
//! ```

use almond_stt::models::ModelManager;
use almond_stt::routing::{DeviceClass, RoutingTable};

fn main() {
    let mgr = ModelManager::open_default().expect("models dir");
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("list") => {
            println!("models dir: {}", mgr.dir.display());
            for m in mgr.list() {
                println!(
                    "{:<34} {:<18} {:>6} MB  {}  {:?}",
                    m.id,
                    m.engine,
                    m.size_bytes.map(|s| s / 1_000_000).unwrap_or(0),
                    if m.installed {
                        "installed"
                    } else {
                        "-        "
                    },
                    m.languages.iter().map(|l| l.0.as_str()).collect::<Vec<_>>()
                );
            }
        }
        Some("get") => {
            #[cfg(feature = "download")]
            for id in &args[1..] {
                let mut last = 0;
                let dir = mgr
                    .ensure(id, &mut |done, total| {
                        let pct = total.map(|t| done * 100 / t.max(1)).unwrap_or(0);
                        if pct >= last + 10 {
                            eprintln!("{id}: {pct}% ({} MB)", done / 1_000_000);
                            last = pct;
                        }
                    })
                    .unwrap_or_else(|e| panic!("{id}: {e}"));
                println!("{id}: installed in {}", dir.display());
            }
            #[cfg(not(feature = "download"))]
            panic!("built without the `download` feature");
        }
        Some("routes") => {
            for d in [
                DeviceClass::DesktopGpu,
                DeviceClass::DesktopCpu,
                DeviceClass::Mobile,
            ] {
                println!(
                    "{}",
                    serde_json::to_string(&RoutingTable::default_for(d)).unwrap()
                );
            }
        }
        Some(other) => panic!("unknown command {other}"),
    }
}
