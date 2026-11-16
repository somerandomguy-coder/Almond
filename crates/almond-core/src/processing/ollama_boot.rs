//! Start this device's Ollama when the app starts and it isn't running (`CoreConfig::start_ollama`),
//! so the installed app works from a single click. Only for a *local* URL (127.0.0.1 / localhost /
//! ::1); a remote Ollama is someone else's to run. The server is left running when the app quits,
//! as Ollama's own tray app does.

use crate::Core;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// `http://127.0.0.1:11434` → the address to probe, if it is a loopback one.
pub(crate) fn local_addr(url: &str) -> Option<SocketAddr> {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next()?;
    let host = authority
        .trim_start_matches('[')
        .rsplit_once(':')
        .map_or(authority, |(h, _)| h)
        .trim_end_matches(']');
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return None;
    }
    authority.to_socket_addrs().ok()?.next()
}

fn reachable(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_ok()
}

/// `ollama` on PATH, else the default install locations.
fn find_ollama() -> Option<PathBuf> {
    let exe = if cfg!(windows) { "ollama.exe" } else { "ollama" };
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>());
    let defaults = [
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Programs").join("Ollama")),
        Some(PathBuf::from("/usr/local/bin")),
        Some(PathBuf::from("/usr/bin")),
    ];
    on_path
        .chain(defaults.into_iter().flatten())
        .map(|d| d.join(exe))
        .find(|p| p.is_file())
}

pub(crate) fn ensure_running(core: &Arc<Core>) {
    if !core.cfg.start_ollama || core.cfg.platform == "android" {
        return;
    }
    let Some(addr) = local_addr(&super::local_ollama_url(core)) else {
        return;
    };
    tokio::task::spawn_blocking(move || {
        if reachable(addr) {
            return;
        }
        let Some(bin) = find_ollama() else {
            tracing::warn!("Ollama is not running and was not found; AI notes need it (https://ollama.com)");
            return;
        };
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg("serve")
            .env("OLLAMA_HOST", addr.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        match cmd.spawn() {
            Ok(_) => tracing::info!("started {} serve on {addr}", bin.display()),
            Err(e) => tracing::warn!("could not start Ollama ({}): {e}", bin.display()),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::local_addr;

    #[test]
    fn only_loopback_urls_are_started() {
        assert!(local_addr("http://127.0.0.1:11434").is_some());
        assert!(local_addr("http://localhost:11434").is_some());
        assert!(local_addr("http://[::1]:11434").is_some());
        assert!(local_addr("http://100.80.179.59:11434").is_none());
        assert!(local_addr("http://pc.tail1234.ts.net:11434").is_none());
    }
}
