//! Shared helpers for almond-core integration tests (`mod common;` in each test file).
#![allow(dead_code)]

use almond_core::{Api, Core, CoreConfig};

/// Open a real `Core` (SQLite store, localhost-only sync, no mDNS) in a fresh temp dir.
pub async fn open_core(name: &str) -> (Api, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "almond-core-test-{name}-{}-{}",
        std::process::id(),
        almond_types::time::now_ms()
    ));
    let mut cfg = CoreConfig::new(&dir, "Test device");
    cfg.network = false;
    (Core::open(cfg).await.expect("open core"), dir)
}
