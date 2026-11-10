use super::*;
use almond_types::mock::MemBlobStore;

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!(
            "almond-rec-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tone_frames(ch: AudioChannel, secs: u32, hz: f32, frame_ms: u64) -> Vec<AudioFrame> {
    let n = (16 * frame_ms) as usize;
    let frames = secs as u64 * 1000 / frame_ms;
    (0..frames)
        .map(|k| AudioFrame {
            channel: ch,
            t0_ms: k * frame_ms,
            samples: (0..n)
                .map(|i| {
                    let t = (k as usize * n + i) as f32 / 16_000.0;
                    0.3 * (2.0 * std::f32::consts::PI * hz * t).sin()
                })
                .collect(),
        })
        .collect()
}

fn blob_to_pcm(blobs: &MemBlobStore, t: &AudioTrack, tmp: &Path) -> Vec<f32> {
    let bytes = blobs.get(&t.blob).unwrap().expect("blob stored");
    let p = tmp.join(format!("decoded-{:?}.{}", t.channel, t.codec));
    std::fs::write(&p, bytes).unwrap();
    let codec = if t.codec == "opus" {
        Codec::Opus
    } else {
        Codec::Wav
    };
    decode_track(&p, codec).unwrap()
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn roundtrip(codec: Codec) {
    let tmp = TempDir::new(codec.as_str());
    let dir = tmp.0.join("session");
    let cfg = RecorderConfig {
        codec,
        chunk_secs: 2,
        ..Default::default()
    };
    let mut rec = SessionRecorder::create(&dir, 1_700_000_000_000, cfg).unwrap();
    let mic = tone_frames(AudioChannel::Mic, 7, 440.0, 100);
    let sys = tone_frames(AudioChannel::System, 7, 1000.0, 100);
    for (a, b) in mic.iter().zip(&sys) {
        rec.push(a).unwrap();
        rec.push(b).unwrap();
    }
    // 4 chunks of <=2 s per channel.
    assert_eq!(
        chunk_files(&dir, AudioChannel::Mic, codec).unwrap().len(),
        4
    );
    let blobs = MemBlobStore::default();
    let tracks = rec.finish(&blobs).unwrap();
    assert_eq!(tracks.len(), 2);
    for t in &tracks {
        assert_eq!(t.duration_ms, 7000, "{t:?}");
        assert_eq!(t.codec, codec.as_str());
        let pcm = blob_to_pcm(&blobs, t, &tmp.0);
        assert_eq!(pcm.len(), 7 * 16_000, "{:?}", t.channel);
        // Lossy codec: compare level, and check it is the right tone via correlation.
        let r = rms(&pcm[1600..]);
        assert!((r - 0.3 / 2f32.sqrt()).abs() < 0.03, "rms {r}");
        let hz = if t.channel == AudioChannel::Mic {
            440.0
        } else {
            1000.0
        };
        let mut dot = 0.0f32;
        let mut norm = 0.0f32;
        for (i, &v) in pcm.iter().enumerate().skip(1600) {
            let r = 0.3 * (2.0 * std::f32::consts::PI * hz * i as f32 / 16_000.0).sin();
            dot += v * r;
            norm += r * r;
        }
        assert!(
            dot / norm > 0.9,
            "{:?} correlation {}",
            t.channel,
            dot / norm
        );
    }
    // Files cleaned up, manifest finished, recovery idempotent.
    assert!(
        chunk_files(&dir, AudioChannel::Mic, codec)
            .unwrap()
            .is_empty()
    );
    assert!(unfinished_sessions(&tmp.0).unwrap().is_empty());
    let again = recover_session(&dir, &blobs, false).unwrap();
    assert_eq!(again, tracks);
}

#[cfg(feature = "opus")]
#[test]
fn opus_roundtrip_multi_chunk() {
    roundtrip(Codec::Opus);
}

#[test]
fn wav_roundtrip_multi_chunk() {
    roundtrip(Codec::Wav);
}

fn crash_and_recover(codec: Codec) {
    let tmp = TempDir::new(&format!("crash-{}", codec.as_str()));
    let dir = tmp.0.join("s1");
    let cfg = RecorderConfig {
        codec,
        chunk_secs: 2,
        ..Default::default()
    };
    let mut rec = SessionRecorder::create(&dir, 0, cfg).unwrap();
    for f in tone_frames(AudioChannel::Mic, 5, 300.0, 100) {
        rec.push(&f).unwrap();
    }
    // Simulated crash: nothing is closed or flushed beyond what already hit the OS.
    std::mem::forget(rec);
    // ...and the last write was torn.
    let chunks = chunk_files(&dir, AudioChannel::Mic, codec).unwrap();
    let last = chunks.last().unwrap();
    let len = std::fs::metadata(last).unwrap().len();
    let f = std::fs::OpenOptions::new().write(true).open(last).unwrap();
    f.set_len(len.saturating_sub(37)).unwrap();
    drop(f);

    assert_eq!(unfinished_sessions(&tmp.0).unwrap(), vec![dir.clone()]);
    let blobs = MemBlobStore::default();
    let tracks = recover_session(&dir, &blobs, false).unwrap();
    assert_eq!(tracks.len(), 1);
    let t = &tracks[0];
    // Lost at most ~1 s (+ the torn bytes) of the 5 s.
    assert!(t.duration_ms >= 3900 && t.duration_ms <= 5000, "{t:?}");
    let pcm = blob_to_pcm(&blobs, t, &tmp.0);
    assert!((pcm.len() as i64 - t.duration_ms as i64 * 16).abs() <= 16 * 20);
    assert!(unfinished_sessions(&tmp.0).unwrap().is_empty());
}

#[cfg(feature = "opus")]
#[test]
fn opus_recovers_after_crash() {
    crash_and_recover(Codec::Opus);
}

#[test]
fn wav_recovers_after_crash() {
    crash_and_recover(Codec::Wav);
}

#[test]
fn gaps_and_overlaps_follow_clock() {
    let tmp = TempDir::new("gaps");
    let dir = tmp.0.join("s");
    let cfg = RecorderConfig {
        codec: Codec::Wav,
        keep_files: true,
        ..Default::default()
    };
    let mut rec = SessionRecorder::create(&dir, 0, cfg).unwrap();
    let f = |t0: u64| AudioFrame {
        channel: AudioChannel::System,
        t0_ms: t0,
        samples: vec![0.5; 1600],
    };
    rec.push(&f(500)).unwrap(); // starts late -> 500 ms silence first
    rec.push(&f(550)).unwrap(); // overlaps by 50 ms -> trimmed
    let blobs = MemBlobStore::default();
    let tracks = rec.finish(&blobs).unwrap();
    assert_eq!(tracks[0].duration_ms, 650); // 500 silence + 100 + 50 non-overlapping
    let pcm = blob_to_pcm(&blobs, &tracks[0], &tmp.0);
    assert!(pcm[..8000].iter().all(|&x| x == 0.0));
    assert!(pcm[8000..].iter().all(|&x| x > 0.49));
}

#[test]
fn refuses_existing_session() {
    let tmp = TempDir::new("exists");
    let _r = SessionRecorder::create(&tmp.0, 0, RecorderConfig::default()).unwrap();
    assert!(SessionRecorder::create(&tmp.0, 0, RecorderConfig::default()).is_err());
}
