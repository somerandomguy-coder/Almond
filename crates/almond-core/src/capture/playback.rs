//! `audio.url`: a URL the webview can play for a meeting's audio.
//!
//! - One channel asked for (or the meeting has one track): that track's blob file.
//! - Otherwise (desktop online meeting: mic + system): a mixed playback file, built on first use
//!   into `playback/<meeting>-<tracks hash>.opus` (Ogg Opus, ~11 MB/h) and cached.
//! - `None` when the audio is not on this device (e.g. a phone that synced text only).
//!
//! The URL uses Tauri's asset protocol (`convertFileSrc` scheme): `http://asset.localhost/<path>`
//! on Windows and Android, `asset://localhost/<path>` elsewhere. The desktop app enables the
//! protocol for `$APPDATA/blobs/**` and `$APPDATA/playback/**` (tauri.conf.json).

use super::transcribe::meeting_audio;
use crate::Core;
use almond_types::meeting::{AudioChannel, AudioTrack};
use almond_types::store::{BlobStore, Store};
use almond_types::{Error, Id, Result};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `encodeURIComponent`, as Tauri's `convertFileSrc` applies to the path.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3 / 2);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Tauri asset-protocol URL for a local file (`convertFileSrc(path)` in the UI).
pub fn asset_url(path: &Path, platform: &str) -> String {
    let enc = encode_uri_component(&path.to_string_lossy());
    if platform == "windows" || platform == "android" {
        format!("http://asset.localhost/{enc}")
    } else {
        format!("asset://localhost/{enc}")
    }
}

fn local_path(blobs: &dyn BlobStore, t: &AudioTrack) -> Result<Option<PathBuf>> {
    if !blobs.has(&t.blob)? {
        return Ok(None);
    }
    blobs.path(&t.blob)
}

/// The file to play, or `None` if the audio is not on this device.
pub(crate) async fn playable_path(
    core: &Arc<Core>,
    meeting: &Id,
    channel: Option<AudioChannel>,
) -> Result<Option<PathBuf>> {
    let m = core
        .store()
        .get_meeting(meeting)?
        .ok_or_else(|| Error::NotFound(format!("meeting {meeting}")))?;
    let blobs = core.blobs().clone();
    let tracks: Vec<AudioTrack> = match channel {
        Some(c) => m.audio.iter().filter(|t| t.channel == c).cloned().collect(),
        None => m.audio.clone(),
    };
    let local: Vec<(AudioTrack, PathBuf)> = tracks
        .iter()
        .filter_map(|t| match local_path(blobs.as_ref(), t) {
            Ok(Some(p)) => Some(Ok((t.clone(), p))),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<Result<_>>()?;
    match local.len() {
        0 => Ok(None),
        1 => Ok(Some(local[0].1.clone())),
        _ => {
            let dir = core.cfg.data_dir.join("playback");
            let id = m.id.clone();
            let tracks: Vec<AudioTrack> = local.into_iter().map(|(t, _)| t).collect();
            tokio::task::spawn_blocking(move || mixed_file(&dir, &id, blobs.as_ref(), &tracks))
                .await
                .map_err(Error::other)?
                .map(Some)
        }
    }
}

/// Mix all tracks onto one timeline and encode it (cached by the set of track blobs).
fn mixed_file(
    dir: &Path,
    meeting: &Id,
    blobs: &dyn BlobStore,
    tracks: &[AudioTrack],
) -> Result<PathBuf> {
    let key = {
        let mut h: Vec<&str> = tracks.iter().map(|t| t.blob.0.as_str()).collect();
        h.sort();
        let joined = h.join(",");
        // short, stable (FNV-1a) — only distinguishes track sets of one meeting
        let mut x: u64 = 0xcbf29ce484222325;
        for b in joined.bytes() {
            x ^= b as u64;
            x = x.wrapping_mul(0x100000001b3);
        }
        format!("{x:016x}")
    };
    let out = dir.join(format!("{}-{key}.opus", meeting.as_str()));
    if out.is_file() {
        return Ok(out);
    }
    std::fs::create_dir_all(dir)?;
    let (audio, _) = meeting_audio(blobs, tracks)?;
    let chans: Vec<&Vec<f32>> = [&audio.mic, &audio.system, &audio.mixed]
        .into_iter()
        .flatten()
        .collect();
    let len = chans.iter().map(|c| c.len()).max().unwrap_or(0);
    let mut mix = vec![0f32; len];
    for c in &chans {
        for (m, x) in mix.iter_mut().zip(c.iter()) {
            *m += *x;
        }
    }
    // soft clip instead of hard clamping where both sides talk at once
    for m in mix.iter_mut() {
        if m.abs() > 0.9 {
            *m = m.signum() * (0.9 + 0.1 * ((m.abs() - 0.9) / 0.1).tanh());
        }
    }
    let tmp = dir.join(format!(".{}-{key}", meeting.as_str()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let mut w = almond_audio::recorder::ogg_opus::OpusChannelWriter::create(
        &tmp,
        "mix",
        u32::MAX / 100_000,
        24_000,
        std::time::Duration::from_secs(3600),
    )?;
    w.push(&mix)?;
    w.finish()?;
    std::fs::rename(tmp.join("mix-00000.opus"), &out)?;
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_urls_match_convert_file_src() {
        assert_eq!(
            asset_url(
                Path::new(r"C:\Users\Nam\AppData\Roaming\dev.almond.app\blobs\ab\cd"),
                "windows"
            ),
            "http://asset.localhost/C%3A%5CUsers%5CNam%5CAppData%5CRoaming%5Cdev.almond.app%5Cblobs%5Cab%5Ccd"
        );
        assert_eq!(
            asset_url(Path::new("/home/nam/.local/share/a b"), "linux"),
            "asset://localhost/%2Fhome%2Fnam%2F.local%2Fshare%2Fa%20b"
        );
    }
}
