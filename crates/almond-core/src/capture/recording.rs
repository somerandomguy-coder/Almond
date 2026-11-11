//! `recording.start` / `recording.stop` / `recording.status`, `audio.devices`, crash recovery.
//!
//! Start: create or update the meeting (`meeting.upsert`, `tz` = local IANA zone), recover any
//! interrupted session, open one stream per channel (mic + system loopback on desktop, mic only
//! on Android), and run them through an `almond_audio::CaptureSession` into crash-safe Opus chunks
//! under `recordings/<meeting>/<started_at_ms>/`. Frames also feed the level meters (status
//! notification every 250 ms) and, when asked, live captions.
//!
//! Stop: finalize the chunks into one Ogg Opus file per channel, `put_file` them into the blob
//! store, append `blob.available` per track and upsert the meeting with its `audio` and
//! `ended_at`. The processing module's job loop then schedules `transcribe.local`.
//!
//! Loopback loss (the PC's HDMI endpoint vanishes when the display sleeps): the cpal backend keeps
//! the system channel alive with zero-filled `synthesized` frames and re-opens the device when it
//! comes back; at start a missing loopback device is waited for instead of failing. The
//! microphone keeps recording throughout; synthesized frames are skipped by live captions and
//! logged as "device lost".

use super::live;
use crate::Core;
use almond_audio::{CaptureSession, RecorderConfig, SessionRecorder};
use almond_types::api::{
    CreateMeetingParams, RecordingState, RecordingStatus, StartRecordingParams, notifications,
};
use almond_types::audio::{AudioBackend, AudioDeviceInfo, AudioFrame, AudioStream, CaptureConfig};
use almond_types::event::EventBody;
use almond_types::meeting::{AudioChannel, AudioTrack, Meeting, MeetingKind};
use almond_types::store::{BlobStore, Store};
use almond_types::time::now_ms;
use almond_types::{Error, Id, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

const STATUS_EVERY: Duration = Duration::from_millis(250);
const FRAME_MS: u32 = 100;
/// Live frames buffered between the capture and the meters / captions (100 ms each).
const LIVE_CAPACITY: usize = 600;
/// A channel delivering only synthesized frames this long counts as "device lost".
const LOST_AFTER_MS: u64 = 2_000;
/// Written next to the recorder's own manifest so recovery knows which meeting a session is for.
const SIDECAR: &str = "almond.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Sidecar {
    meeting_id: Id,
    /// Track offset from `Meeting::started_at`.
    offset_ms: u32,
    started_at: i64,
}

/// Status shared between the API, the meter task and the ticker.
#[derive(Debug, Default)]
struct Status {
    state: Option<RecordingState>,
    meeting_id: Option<Id>,
    started_at: Option<i64>,
    started: Option<Instant>,
    /// elapsed at stop (shown after the recording ended)
    final_elapsed_ms: u32,
    levels: Vec<(AudioChannel, f32)>,
    /// consecutive synthesized audio per channel (ms) and whether it counts as lost
    synth_ms: Vec<(AudioChannel, u64, bool)>,
}

impl Status {
    fn snapshot(&self) -> RecordingStatus {
        let state = self.state.unwrap_or(RecordingState::Idle);
        let elapsed_ms = match (state, self.started) {
            (RecordingState::Recording, Some(t)) => t.elapsed().as_millis() as u32,
            _ => self.final_elapsed_ms,
        };
        RecordingStatus {
            state,
            meeting_id: self.meeting_id.clone(),
            started_at: self.started_at,
            elapsed_ms,
            levels: if state == RecordingState::Recording {
                self.levels.clone()
            } else {
                vec![]
            },
        }
    }

    fn on_frame(&mut self, f: &AudioFrame) {
        // Meter value: RMS mapped from [-60, 0] dBFS to [0, 1] (raw speech RMS is ~0.05, which
        // would barely move a linear meter).
        let level = if f.synthesized {
            0.0
        } else {
            ((almond_audio::level::rms_dbfs(&f.samples) + 60.0) / 60.0).clamp(0.0, 1.0)
        };
        match self.levels.iter_mut().find(|(c, _)| *c == f.channel) {
            // fast attack, slower release
            Some((_, l)) => {
                *l = if level > *l {
                    level
                } else {
                    *l * 0.6 + level * 0.4
                }
            }
            None => self.levels.push((f.channel, level)),
        }
        let ms = f.samples.len() as u64 * 1000 / almond_types::audio::PIPELINE_SAMPLE_RATE as u64;
        let i = match self.synth_ms.iter().position(|(c, _, _)| *c == f.channel) {
            Some(i) => i,
            None => {
                self.synth_ms.push((f.channel, 0, false));
                self.synth_ms.len() - 1
            }
        };
        let (ch, run, lost) = &mut self.synth_ms[i];
        if f.synthesized {
            *run += ms;
            if *run >= LOST_AFTER_MS && !*lost {
                *lost = true;
                tracing::warn!(channel = ?ch, t_ms = f.t0_ms, "audio device lost; recording continues on the other channels");
            }
        } else {
            if *lost {
                tracing::info!(channel = ?ch, t_ms = f.t0_ms, "audio device back");
            }
            *run = 0;
            *lost = false;
        }
    }
}

struct Active {
    meeting_id: Id,
    session: CaptureSession,
    dir: PathBuf,
    offset_ms: u32,
    stop_ticker: watch::Sender<bool>,
    ticker: JoinHandle<()>,
    meter: JoinHandle<()>,
    live: Vec<JoinHandle<()>>,
}

pub(crate) struct Recorder {
    active: tokio::sync::Mutex<Option<Active>>,
    status: Arc<Mutex<Status>>,
    backend: RwLock<Option<Arc<dyn AudioBackend>>>,
    #[cfg(not(target_os = "android"))]
    cpal: Arc<almond_audio::CpalBackend>,
}

impl Recorder {
    pub(crate) fn new() -> Self {
        Recorder {
            active: Default::default(),
            status: Default::default(),
            backend: Default::default(),
            #[cfg(not(target_os = "android"))]
            cpal: Arc::new(almond_audio::CpalBackend::new()),
        }
    }

    pub(crate) fn set_backend(&self, b: Option<Arc<dyn AudioBackend>>) {
        *self.backend.write().unwrap_or_else(|p| p.into_inner()) = b;
    }

    fn custom_backend(&self) -> Option<Arc<dyn AudioBackend>> {
        self.backend
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub(crate) fn status(&self) -> RecordingStatus {
        self.status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot()
    }

    pub(crate) async fn list_devices(&self) -> Result<Vec<AudioDeviceInfo>> {
        if let Some(b) = self.custom_backend() {
            return b.list_devices().await;
        }
        self.native_devices().await
    }

    /// App shutdown: stop the devices without finalizing; the chunks on disk are recovered into
    /// the meeting at the next start.
    pub(crate) async fn abort(&self) {
        if let Some(a) = self.active.lock().await.take() {
            let _ = a.stop_ticker.send(true);
            a.ticker.abort();
            for t in &a.live {
                t.abort();
            }
            drop(a.session);
            a.meter.abort();
            tracing::info!(meeting = %a.meeting_id, "recording interrupted by shutdown; will be recovered");
        }
    }

    pub(crate) async fn is_recording(&self) -> bool {
        self.active.lock().await.is_some()
    }

    /// Open one stream per channel. Returns the streams and the wall-clock time (unix ms) of
    /// their common `t0_ms = 0`.
    pub(super) async fn open(
        &self,
        channels: &[AudioChannel],
        p: &StartRecordingParams,
    ) -> Result<(Vec<Box<dyn AudioStream>>, i64)> {
        let cfg_for = |ch: AudioChannel| CaptureConfig {
            device_id: match ch {
                AudioChannel::System => p.system_device.clone(),
                _ => p.mic_device.clone(),
            },
            frame_ms: FRAME_MS,
        };
        if let Some(b) = self.custom_backend() {
            let epoch = now_ms();
            let mut out = vec![];
            for &ch in channels {
                match b.open(ch, cfg_for(ch)).await {
                    Ok(s) => out.push(s),
                    Err(e) if ch == AudioChannel::System && channels.len() > 1 => {
                        tracing::warn!("system audio unavailable, recording without it: {e}")
                    }
                    Err(e) => return Err(e),
                }
            }
            return Ok((out, epoch));
        }
        let cfgs: Vec<(AudioChannel, CaptureConfig)> =
            channels.iter().map(|c| (*c, cfg_for(*c))).collect();
        self.open_native(cfgs).await
    }

    #[cfg(not(target_os = "android"))]
    async fn open_native(
        &self,
        cfgs: Vec<(AudioChannel, CaptureConfig)>,
    ) -> Result<(Vec<Box<dyn AudioStream>>, i64)> {
        let cpal = self.cpal.clone();
        tokio::task::spawn_blocking(move || {
            let clock = cpal.reset_clock();
            let mut out: Vec<Box<dyn AudioStream>> = vec![];
            for (ch, cfg) in &cfgs {
                let s = if *ch == AudioChannel::System {
                    // A missing loopback endpoint (display asleep) must not stop the mic.
                    cpal.open_stream_waiting(*ch, cfg)?
                } else {
                    cpal.open_stream(*ch, cfg)?
                };
                out.push(Box::new(s));
            }
            Ok((out, clock.epoch_unix_ms() as i64))
        })
        .await
        .map_err(Error::other)?
    }

    #[cfg(target_os = "android")]
    async fn open_native(
        &self,
        _cfgs: Vec<(AudioChannel, CaptureConfig)>,
    ) -> Result<(Vec<Box<dyn AudioStream>>, i64)> {
        Err(Error::Unsupported(
            "Android capture goes through the Kotlin AudioRecord bridge (PushAudioSource), \
             which is not wired into the core yet"
                .into(),
        ))
    }

    #[cfg(not(target_os = "android"))]
    async fn native_devices(&self) -> Result<Vec<AudioDeviceInfo>> {
        self.cpal.list_devices().await
    }

    #[cfg(target_os = "android")]
    async fn native_devices(&self) -> Result<Vec<AudioDeviceInfo>> {
        Ok(vec![])
    }
}

fn recordings_root(core: &Core) -> PathBuf {
    core.cfg.data_dir.join("recordings")
}

fn local_tz() -> Option<String> {
    iana_time_zone::get_timezone().ok()
}

fn default_kind(platform: &str) -> MeetingKind {
    if platform == "android" {
        MeetingKind::InPerson
    } else {
        MeetingKind::Online { app: None }
    }
}

fn new_meeting(core: &Core, id: Id, create: Option<&CreateMeetingParams>) -> Meeting {
    Meeting {
        id,
        title: create.and_then(|c| c.title.clone()).unwrap_or_default(),
        started_at: now_ms(),
        ended_at: None,
        kind: create
            .map(|c| c.kind.clone())
            .unwrap_or_else(|| default_kind(&core.cfg.platform)),
        calendar_event: create.and_then(|c| c.calendar_event.clone()),
        languages: vec![],
        audio: vec![],
        notes_doc: Id::new(),
        tags: vec![],
        tz: local_tz(),
    }
}

/// The meeting to record into: the given one (created if unknown here), else a new one.
fn ensure_meeting(core: &Core, p: &StartRecordingParams) -> Result<Meeting> {
    let store = core.store();
    let m = match &p.meeting_id {
        Some(id) => match store.get_meeting(id)? {
            Some(mut m) => {
                if m.tz.is_none() {
                    m.tz = local_tz();
                    store.append_local(EventBody::MeetingUpsert(m.clone()))?;
                }
                return Ok(m);
            }
            None => new_meeting(core, id.clone(), p.create.as_ref()),
        },
        None => new_meeting(core, Id::new(), p.create.as_ref()),
    };
    store.append_local(EventBody::MeetingUpsert(m.clone()))?;
    Ok(m)
}

fn channels_for(core: &Core, p: &StartRecordingParams) -> Vec<AudioChannel> {
    let android = core.cfg.platform == "android";
    let mut v: Vec<AudioChannel> = if p.channels.is_empty() {
        if android {
            vec![AudioChannel::Mic]
        } else {
            vec![AudioChannel::Mic, AudioChannel::System]
        }
    } else {
        p.channels.clone()
    };
    if android {
        v.retain(|c| *c != AudioChannel::System);
    }
    let mut seen = vec![];
    v.retain(|c| {
        let new = !seen.contains(c);
        seen.push(*c);
        new
    });
    v
}

/// Setting: language of live captions (`"vi"` | `"en"`), see `start`.
pub(crate) const CAPTIONS_LANGUAGE: &str = "captions.language";

pub(crate) async fn start(
    core: &Arc<Core>,
    mut p: StartRecordingParams,
) -> Result<RecordingStatus> {
    // The mic test holds the device and resets the capture clock: stop it first.
    super::mictest::stop(core).await;
    if p.mic_device.is_none() {
        p.mic_device = super::preferred_mic(core);
    }
    let rec = &core.capture.rec;
    let mut active = rec.active.lock().await;
    if let Some(a) = active.as_ref() {
        return Err(Error::Invalid(format!(
            "already recording meeting {}",
            a.meeting_id
        )));
    }
    if let Err(e) = recover_locked(core, None).await {
        tracing::warn!("recovering unfinished recordings: {e}");
    }
    let channels = channels_for(core, &p);
    if channels.is_empty() {
        return Err(Error::Invalid("no audio channel to record".into()));
    }
    let meeting = ensure_meeting(core, &p)?;
    let (streams, epoch) = rec.open(&channels, &p).await?;
    if streams.is_empty() {
        return Err(Error::NotFound("no audio device could be opened".into()));
    }
    let offset_ms = (epoch - meeting.started_at).clamp(0, u32::MAX as i64) as u32;
    let dir = recordings_root(core)
        .join(meeting.id.as_str())
        .join(epoch.to_string());
    let recorder = {
        let dir = dir.clone();
        let side = Sidecar {
            meeting_id: meeting.id.clone(),
            offset_ms,
            started_at: epoch,
        };
        tokio::task::spawn_blocking(move || -> Result<SessionRecorder> {
            let r = SessionRecorder::create(&dir, epoch.max(0) as u64, RecorderConfig::default())?;
            std::fs::write(dir.join(SIDECAR), serde_json::to_vec_pretty(&side)?)?;
            Ok(r)
        })
        .await
        .map_err(Error::other)??
    };
    let (session, frames) = CaptureSession::start(streams, Some(recorder), LIVE_CAPACITY)?;

    {
        let mut st = rec.status.lock().unwrap_or_else(|p| p.into_inner());
        *st = Status {
            state: Some(RecordingState::Recording),
            meeting_id: Some(meeting.id.clone()),
            started_at: Some(epoch),
            started: Some(Instant::now()),
            ..Default::default()
        };
    }

    // Live captions, one stream per channel.
    let (feeds, live_tasks) = if p.live_transcript {
        // Setting `captions.language` ("vi" | "en") wins; else the meeting's language.
        let lang = core
            .setting_as::<String>(CAPTIONS_LANGUAGE)
            .ok()
            .flatten()
            .filter(|l| !l.trim().is_empty())
            .map(|l| almond_types::LangTag(l.trim().to_string()))
            .or_else(|| meeting.languages.first().cloned())
            .unwrap_or_else(super::engines::default_language);
        live::spawn(core, &meeting.id, offset_ms, &channels, lang)
    } else {
        (vec![], vec![])
    };
    let meter = tokio::spawn(meter_loop(frames, rec.status.clone(), feeds));
    let (stop_ticker, stop_rx) = watch::channel(false);
    let ticker = tokio::spawn(ticker_loop(
        core.notifier.clone(),
        rec.status.clone(),
        stop_rx,
    ));
    tracing::info!(meeting = %meeting.id, ?channels, dir = %dir.display(), "recording started");
    *active = Some(Active {
        meeting_id: meeting.id.clone(),
        session,
        dir,
        offset_ms,
        stop_ticker,
        ticker,
        meter,
        live: live_tasks,
    });
    let s = rec.status();
    core.notifier.emit(notifications::RECORDING_STATUS, &s);
    Ok(s)
}

async fn meter_loop(
    mut frames: mpsc::Receiver<AudioFrame>,
    status: Arc<Mutex<Status>>,
    feeds: Vec<(AudioChannel, mpsc::Sender<AudioFrame>)>,
) {
    let mut dropped = 0u64;
    while let Some(f) = frames.recv().await {
        status
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .on_frame(&f);
        if f.synthesized {
            continue; // zero-fill (device gap / lost loopback): nothing to caption
        }
        if let Some((_, tx)) = feeds.iter().find(|(c, _)| *c == f.channel)
            && tx.try_send(f).is_err()
        {
            dropped += 1;
            if dropped.is_power_of_two() {
                tracing::warn!(dropped, "live captions are falling behind; frames skipped");
            }
        }
    }
    // dropping `feeds` ends the caption streams (they flush their finals)
}

async fn ticker_loop(
    notifier: crate::Notifier,
    status: Arc<Mutex<Status>>,
    mut stop: watch::Receiver<bool>,
) {
    let mut iv = tokio::time::interval(STATUS_EVERY);
    loop {
        tokio::select! {
            _ = iv.tick() => {}
            _ = stop.wait_for(|s| *s) => break,
        }
        let s = status.lock().unwrap_or_else(|p| p.into_inner()).snapshot();
        notifier.emit(notifications::RECORDING_STATUS, &s);
    }
}

pub(crate) async fn stop(core: &Arc<Core>) -> Result<RecordingStatus> {
    let rec = &core.capture.rec;
    let mut guard = rec.active.lock().await;
    let Some(a) = guard.take() else {
        return Ok(rec.status());
    };
    {
        let mut st = rec.status.lock().unwrap_or_else(|p| p.into_inner());
        st.final_elapsed_ms = st
            .started
            .map(|t| t.elapsed().as_millis() as u32)
            .unwrap_or(0);
        st.state = Some(RecordingState::Finalizing);
    }
    let _ = a.stop_ticker.send(true);
    let _ = a.ticker.await;
    core.notifier
        .emit(notifications::RECORDING_STATUS, rec.status());

    let blobs = core.blobs().clone();
    let summary = a.session.stop(blobs.as_ref()).await;
    // Let the caption streams flush their last finals (bounded).
    let _ = tokio::time::timeout(Duration::from_secs(5), a.meter).await;
    for t in a.live {
        if tokio::time::timeout(Duration::from_secs(10), t)
            .await
            .is_err()
        {
            tracing::warn!("live captions did not finish in time");
        }
    }
    let result = match summary {
        Ok(summary) => {
            for e in &summary.errors {
                tracing::warn!(meeting = %a.meeting_id, "capture: {e}");
            }
            if summary.live_frames_dropped > 0 {
                tracing::info!(
                    dropped = summary.live_frames_dropped,
                    "live frames dropped (recording unaffected)"
                );
            }
            let ended = now_ms();
            attach_tracks(core, &a.meeting_id, summary.tracks, a.offset_ms, ended).await
        }
        Err(e) => Err(e),
    };
    {
        let mut st = rec.status.lock().unwrap_or_else(|p| p.into_inner());
        st.state = Some(RecordingState::Idle);
        st.levels.clear();
    }
    let s = rec.status();
    core.notifier.emit(notifications::RECORDING_STATUS, &s);
    match result {
        Ok(n) => {
            tracing::info!(meeting = %a.meeting_id, tracks = n, "recording finished");
            remove_session_dir(&a.dir);
            Ok(s)
        }
        Err(e) => {
            tracing::error!(meeting = %a.meeting_id, dir = %a.dir.display(), "finalizing recording failed (chunks kept for recovery): {e}");
            Err(e)
        }
    }
}

fn remove_session_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
    if let Some(parent) = dir.parent() {
        let _ = std::fs::remove_dir(parent); // only if empty
    }
}

/// Announce the tracks' blobs and add them to the meeting. Returns the number of tracks.
pub(crate) async fn attach_tracks(
    core: &Arc<Core>,
    meeting_id: &Id,
    mut tracks: Vec<AudioTrack>,
    offset_ms: u32,
    ended_at: i64,
) -> Result<usize> {
    if tracks.is_empty() {
        return Err(Error::Other("the recording produced no audio".into()));
    }
    let core2 = core.clone();
    let meeting_id = meeting_id.clone();
    tokio::task::spawn_blocking(move || -> Result<usize> {
        let core = core2;
        let store = core.store();
        let blobs = core.blobs();
        for t in tracks.iter_mut() {
            t.offset_ms = offset_ms;
            let size = match blobs.path(&t.blob)? {
                Some(p) => std::fs::metadata(p)?.len(),
                None => blobs.get(&t.blob)?.map(|b| b.len() as u64).unwrap_or(0),
            };
            let mime = match t.codec.as_str() {
                "wav" => "audio/wav",
                "flac" => "audio/flac",
                _ => "audio/ogg",
            };
            store.append_local(EventBody::BlobAvailable {
                hash: t.blob.clone(),
                size,
                mime: mime.into(),
            })?;
        }
        let mut m = match store.get_meeting(&meeting_id)? {
            Some(m) => m,
            None => {
                // The meeting was deleted (or never synced here): keep the audio anyway.
                let mut m = new_meeting(&core, meeting_id.clone(), None);
                m.started_at = ended_at
                    - (tracks.iter().map(|t| t.duration_ms).max().unwrap_or(0) as i64
                        + offset_ms as i64);
                m
            }
        };
        let n = tracks.len();
        m.audio.extend(tracks);
        m.ended_at = Some(m.ended_at.unwrap_or(0).max(ended_at));
        if m.tz.is_none() {
            m.tz = local_tz();
        }
        store.append_local(EventBody::MeetingUpsert(m))?;
        Ok(n)
    })
    .await
    .map_err(Error::other)?
}

/// Recover sessions a crash interrupted (core start-up). Skips the session being recorded.
pub(crate) async fn recover_unfinished(core: &Arc<Core>) -> Result<usize> {
    let active = core.capture.rec.active.lock().await;
    let skip = active.as_ref().map(|a| a.dir.clone());
    recover_locked(core, skip.as_deref()).await
}

/// Caller holds the `active` lock.
async fn recover_locked(core: &Arc<Core>, skip: Option<&Path>) -> Result<usize> {
    let root = recordings_root(core);
    let skip = skip.map(Path::to_path_buf);
    let sessions = tokio::task::spawn_blocking(move || -> Result<Vec<PathBuf>> {
        let mut v = vec![];
        if !root.exists() {
            return Ok(v);
        }
        for e in std::fs::read_dir(&root)? {
            let p = e?.path();
            if p.is_dir() {
                v.extend(almond_audio::unfinished_sessions(&p)?);
            }
        }
        v.retain(|p| Some(p) != skip.as_ref());
        Ok(v)
    })
    .await
    .map_err(Error::other)??;
    let mut n = 0;
    for dir in sessions {
        match recover_one(core, &dir).await {
            Ok(tracks) => {
                tracing::info!(dir = %dir.display(), tracks, "recovered interrupted recording");
                remove_session_dir(&dir);
                n += 1;
            }
            Err(e) => tracing::warn!(dir = %dir.display(), "could not recover recording: {e}"),
        }
    }
    Ok(n)
}

async fn recover_one(core: &Arc<Core>, dir: &Path) -> Result<usize> {
    let blobs = core.blobs().clone();
    let d = dir.to_path_buf();
    let (side, tracks) = tokio::task::spawn_blocking(move || -> Result<_> {
        let side: Option<Sidecar> = std::fs::read(d.join(SIDECAR))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let tracks = almond_audio::recover_session(&d, blobs.as_ref(), false)?;
        Ok((side, tracks))
    })
    .await
    .map_err(Error::other)??;
    let (meeting_id, offset_ms, started_at) = match side {
        Some(s) => (s.meeting_id, s.offset_ms, s.started_at),
        None => {
            // No sidecar: the directory layout still names the meeting.
            let id = dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| Id(n.to_string_lossy().into_owned()))
                .ok_or_else(|| Error::Invalid("unexpected recording directory".into()))?;
            let started = dir
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.parse().ok())
                .unwrap_or_else(now_ms);
            (id, 0, started)
        }
    };
    let dur = tracks.iter().map(|t| t.duration_ms).max().unwrap_or(0) as i64;
    if tracks.is_empty() {
        return Ok(0);
    }
    attach_tracks(core, &meeting_id, tracks, offset_ms, started_at + dur).await
}
