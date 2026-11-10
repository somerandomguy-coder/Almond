//! Live captions: one `SttStream` per recorded channel, fed the captured (non-synthesized)
//! frames, emitting `transcript.update { meeting_id, update }`.
//!
//! The streams number their segments independently, so segment indices are remapped to one
//! meeting-wide sequence (first seen = next index). The `SttUpdate` protocol carries over: a
//! partial for index N replaces the previous partial for N, exactly one final arrives per N.
//! Times are shifted to the meeting timeline (track offset), mic segments are labelled "me".

use super::engines::Speech;
use crate::Core;
use almond_stt::transcript::ME;
use almond_types::api::{TranscriptUpdate, notifications};
use almond_types::audio::AudioFrame;
use almond_types::ids::SpeakerId;
use almond_types::meeting::AudioChannel;
use almond_types::stt::{SttOptions, SttUpdate};
use almond_types::{Error, Id, LangTag};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Frames buffered per caption stream (100 ms each) while its model loads or decodes.
const FEED_CAPACITY: usize = 300;

#[derive(Default)]
struct IdxMap {
    next: u32,
    map: HashMap<(AudioChannel, u32), u32>,
}

impl IdxMap {
    fn global(&mut self, ch: AudioChannel, local: u32) -> u32 {
        *self.map.entry((ch, local)).or_insert_with(|| {
            self.next += 1;
            self.next - 1
        })
    }
}

pub(crate) type Feeds = Vec<(AudioChannel, mpsc::Sender<AudioFrame>)>;

/// Start caption workers for `channels`. Dropping the returned senders ends the streams (they
/// flush their last finals); the handles finish after that.
pub(crate) fn spawn(
    core: &Arc<Core>,
    meeting_id: &Id,
    offset_ms: u32,
    channels: &[AudioChannel],
    lang: LangTag,
) -> (Feeds, Vec<JoinHandle<()>>) {
    let idx = Arc::new(Mutex::new(IdxMap::default()));
    let mut feeds = vec![];
    let mut tasks = vec![];
    for &ch in channels {
        let (tx, rx) = mpsc::channel(FEED_CAPACITY);
        feeds.push((ch, tx));
        tasks.push(tokio::spawn(worker(
            core.capture.speech.clone(),
            core.notifier.clone(),
            meeting_id.clone(),
            ch,
            offset_ms,
            lang.clone(),
            rx,
            idx.clone(),
        )));
    }
    (feeds, tasks)
}

#[allow(clippy::too_many_arguments)]
async fn worker(
    speech: Arc<Speech>,
    notifier: crate::Notifier,
    meeting_id: Id,
    channel: AudioChannel,
    offset_ms: u32,
    lang: LangTag,
    mut rx: mpsc::Receiver<AudioFrame>,
    idx: Arc<Mutex<IdxMap>>,
) {
    let l2 = lang.clone();
    let engine = match tokio::task::spawn_blocking(move || speech.live(&l2))
        .await
        .map_err(Error::other)
    {
        Ok(Ok(e)) => e,
        Ok(Err(e)) | Err(e) => {
            tracing::warn!(?channel, "live captions unavailable: {e}");
            while rx.recv().await.is_some() {}
            return;
        }
    };
    let opts = SttOptions {
        language: Some(lang.clone()),
        allowed_languages: vec![lang],
        ..Default::default()
    };
    let mut stream = match engine.start_stream(opts) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(?channel, "live captions unavailable: {e}");
            while rx.recv().await.is_some() {}
            return;
        }
    };
    let emit = |ups: Vec<SttUpdate>| {
        for u in ups {
            let update = stamp(u, channel, offset_ms, &idx);
            notifier.emit(
                notifications::TRANSCRIPT_UPDATE,
                TranscriptUpdate {
                    meeting_id: meeting_id.clone(),
                    update,
                },
            );
        }
    };
    let mut errors = 0u32;
    while let Some(f) = rx.recv().await {
        match stream.push(f).await {
            Ok(ups) => emit(ups),
            Err(e) => {
                errors += 1;
                if errors.is_power_of_two() {
                    tracing::warn!(?channel, errors, "live caption decode failed: {e}");
                }
            }
        }
    }
    match stream.finish().await {
        Ok(ups) => emit(ups),
        Err(e) => tracing::warn!(?channel, "live captions: {e}"),
    }
}

fn stamp(
    mut u: SttUpdate,
    channel: AudioChannel,
    offset_ms: u32,
    idx: &Mutex<IdxMap>,
) -> SttUpdate {
    let s = &mut u.segment;
    s.idx = idx
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .global(channel, s.idx);
    s.channel = channel;
    s.t0_ms = s.t0_ms.saturating_add(offset_ms);
    s.t1_ms = s.t1_ms.saturating_add(offset_ms);
    if let Some(ws) = s.words.as_mut() {
        for w in ws {
            w.t0_ms = w.t0_ms.saturating_add(offset_ms);
            w.t1_ms = w.t1_ms.saturating_add(offset_ms);
        }
    }
    if channel == AudioChannel::Mic && s.speaker.is_none() {
        s.speaker = Some(SpeakerId(ME.into()));
    }
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indices_are_meeting_wide_and_stable() {
        let mut m = IdxMap::default();
        assert_eq!(m.global(AudioChannel::Mic, 0), 0);
        assert_eq!(m.global(AudioChannel::System, 0), 1);
        assert_eq!(m.global(AudioChannel::Mic, 0), 0);
        assert_eq!(m.global(AudioChannel::Mic, 1), 2);
        assert_eq!(m.global(AudioChannel::System, 0), 1);
    }
}
