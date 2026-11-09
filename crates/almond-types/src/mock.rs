//! In-memory implementations of every trait so workstreams can build and test in isolation.
//! Enabled with the `mock` feature. Not for production use.

use crate::artifact::body::{Claim, Segment, Verdict, VerdictLabel};
use crate::artifact::{Artifact, ArtifactDraft};
use crate::audio::*;
use crate::calendar::*;
use crate::event::{Event, EventBody, EventKind};
use crate::ids::{BlobHash, DeviceId, Id};
use crate::job::{Job, JobStatus};
use crate::lang::LangTag;
use crate::llm::*;
use crate::meeting::{AudioChannel, Meeting};
use crate::pipeline::*;
use crate::secrets::SecretStore;
use crate::store::*;
use crate::stt::*;
use crate::sync::*;
use crate::time::{HlcClock, UnixMs};
use crate::verify::*;
use crate::{Error, Result};
use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ store

#[derive(Default)]
struct MemState {
    log: Vec<Event>,
    seen: std::collections::HashSet<(DeviceId, u64)>,
    vv: VersionVector,
    meetings: BTreeMap<Id, (Meeting, crate::time::Hlc, DeviceId)>,
    artifacts: BTreeMap<Id, Artifact>,
    notes: HashMap<Id, Vec<Vec<u8>>>,
    calendar: BTreeMap<(String, String), CalendarEvent>,
    jobs: BTreeMap<Id, (Job, JobStatus)>,
    kv: HashMap<String, String>,
    next_seq: u64,
}

/// In-memory `Store`. Materializes the well-known event kinds.
pub struct MemStore {
    device: DeviceId,
    clock: HlcClock,
    st: Mutex<MemState>,
}

impl MemStore {
    pub fn new(device: DeviceId) -> Self {
        MemStore {
            device,
            clock: HlcClock::new(),
            st: Mutex::new(MemState::default()),
        }
    }

    fn materialize(st: &mut MemState, ev: &Event) -> Result<()> {
        let Some(body) = EventBody::decode(ev)? else {
            return Ok(());
        };
        match body {
            EventBody::MeetingUpsert(m) => {
                let newer = match st.meetings.get(&m.id) {
                    Some((_, h, d)) => (ev.hlc, &ev.device) > (*h, d),
                    None => true,
                };
                if newer {
                    st.meetings
                        .insert(m.id.clone(), (m, ev.hlc, ev.device.clone()));
                }
            }
            EventBody::MeetingDelete { id } => {
                st.meetings.remove(&id);
            }
            EventBody::ArtifactAdd(a) => {
                st.artifacts.insert(a.id.clone(), a);
            }
            EventBody::NotesUpdate { doc, update_b64 } => {
                st.notes
                    .entry(doc)
                    .or_default()
                    .push(update_b64.into_bytes()); // mock: keeps base64 text, real store decodes
            }
            EventBody::CalendarUpsert(e) => {
                st.calendar
                    .insert((e.source.clone(), e.event_id.clone()), e);
            }
            EventBody::CalendarDelete { source, event_id } => {
                st.calendar.remove(&(source, event_id));
            }
            EventBody::JobRequested(j) => {
                st.jobs
                    .entry(j.id.clone())
                    .or_insert((j, JobStatus::Pending));
            }
            EventBody::JobClaimed(l) => {
                if let Some(j) = st.jobs.get_mut(&l.job_id)
                    && j.1 == JobStatus::Pending
                {
                    j.1 = JobStatus::Claimed;
                }
            }
            EventBody::JobFinished(o) => {
                if let Some(j) = st.jobs.get_mut(&o.job_id) {
                    j.1 = o.status;
                }
            }
            EventBody::DeviceInfo(_) | EventBody::BlobAvailable { .. } => {}
        }
        Ok(())
    }
}

impl Store for MemStore {
    fn device_id(&self) -> DeviceId {
        self.device.clone()
    }

    fn append_local(&self, body: EventBody) -> Result<Event> {
        let mut st = self.st.lock().unwrap();
        st.next_seq += 1;
        let ev = Event {
            device: self.device.clone(),
            seq: st.next_seq,
            hlc: self.clock.now(),
            kind: EventKind(body.kind().to_string()),
            body: body.to_json()?,
        };
        st.seen.insert((ev.device.clone(), ev.seq));
        st.vv.observe(&ev.device, ev.seq);
        Self::materialize(&mut st, &ev)?;
        st.log.push(ev.clone());
        Ok(ev)
    }

    // NOTE: the mock assumes events arrive gap-free per author (true for `events_missing` output).
    // The real store must only advance the version vector over *contiguous* seqs.
    fn apply_remote(&self, events: Vec<Event>) -> Result<usize> {
        let mut st = self.st.lock().unwrap();
        let mut n = 0;
        for ev in events {
            if !st.seen.insert((ev.device.clone(), ev.seq)) {
                continue;
            }
            self.clock.observe(ev.hlc);
            st.vv.observe(&ev.device, ev.seq);
            Self::materialize(&mut st, &ev)?;
            st.log.push(ev);
            n += 1;
        }
        Ok(n)
    }

    fn version_vector(&self) -> Result<VersionVector> {
        Ok(self.st.lock().unwrap().vv.clone())
    }

    fn events_missing(&self, have: &VersionVector, limit: usize) -> Result<Vec<Event>> {
        let st = self.st.lock().unwrap();
        let mut out: Vec<Event> = st
            .log
            .iter()
            .filter(|e| e.seq > have.get(&e.device))
            .cloned()
            .collect();
        out.sort_by(|a, b| (&a.device, a.seq).cmp(&(&b.device, b.seq)));
        out.truncate(limit);
        Ok(out)
    }

    fn get_meeting(&self, id: &Id) -> Result<Option<Meeting>> {
        Ok(self
            .st
            .lock()
            .unwrap()
            .meetings
            .get(id)
            .map(|x| x.0.clone()))
    }

    fn list_meetings(&self, q: &MeetingQuery) -> Result<Vec<Meeting>> {
        let st = self.st.lock().unwrap();
        let mut v: Vec<Meeting> = st
            .meetings
            .values()
            .map(|x| x.0.clone())
            .filter(|m| q.from.is_none_or(|f| m.started_at >= f))
            .filter(|m| q.to.is_none_or(|t| m.started_at < t))
            .filter(|m| q.tag.as_ref().is_none_or(|t| m.tags.contains(t)))
            .filter(|m| {
                q.text
                    .as_ref()
                    .is_none_or(|t| m.title.to_lowercase().contains(&t.to_lowercase()))
            })
            .collect();
        v.sort_by_key(|m| std::cmp::Reverse(m.started_at));
        let off = q.offset.unwrap_or(0) as usize;
        let lim = q.limit.unwrap_or(u32::MAX) as usize;
        Ok(v.into_iter().skip(off).take(lim).collect())
    }

    fn get_artifact(&self, id: &Id) -> Result<Option<Artifact>> {
        Ok(self.st.lock().unwrap().artifacts.get(id).cloned())
    }

    fn list_artifacts(&self, meeting: &Id, kind: Option<&str>) -> Result<Vec<Artifact>> {
        let st = self.st.lock().unwrap();
        let mut v: Vec<Artifact> = st
            .artifacts
            .values()
            .filter(|a| &a.meeting_id == meeting && kind.is_none_or(|k| a.kind.is(k)))
            .cloned()
            .collect();
        v.sort_by_key(|a| std::cmp::Reverse(a.created_at));
        Ok(v)
    }

    fn notes_updates(&self, doc: &Id) -> Result<Vec<Vec<u8>>> {
        Ok(self
            .st
            .lock()
            .unwrap()
            .notes
            .get(doc)
            .cloned()
            .unwrap_or_default())
    }

    fn calendar_events(&self, from: UnixMs, to: UnixMs) -> Result<Vec<CalendarEvent>> {
        let st = self.st.lock().unwrap();
        let mut v: Vec<_> = st
            .calendar
            .values()
            .filter(|e| e.end >= from && e.start < to)
            .cloned()
            .collect();
        v.sort_by_key(|e| e.start);
        Ok(v)
    }

    fn jobs(&self, status: Option<JobStatus>) -> Result<Vec<(Job, JobStatus)>> {
        Ok(self
            .st
            .lock()
            .unwrap()
            .jobs
            .values()
            .filter(|(_, s)| status.is_none_or(|x| x == *s))
            .cloned()
            .collect())
    }

    fn search(&self, text: &str, limit: u32) -> Result<Vec<SearchHit>> {
        let needle = text.to_lowercase();
        let st = self.st.lock().unwrap();
        let mut hits = vec![];
        for a in st.artifacts.values() {
            let s = a.body.to_string().to_lowercase();
            if s.contains(&needle) {
                hits.push(SearchHit {
                    meeting_id: a.meeting_id.clone(),
                    artifact_id: Some(a.id.clone()),
                    kind: a.kind.0.clone(),
                    snippet: text.to_string(),
                    segment: None,
                    score: 1.0,
                });
            }
        }
        hits.truncate(limit as usize);
        Ok(hits)
    }

    fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self.st.lock().unwrap().kv.get(key).cloned())
    }

    fn kv_set(&self, key: &str, value: &str) -> Result<()> {
        self.st.lock().unwrap().kv.insert(key.into(), value.into());
        Ok(())
    }
}

// ------------------------------------------------------------------ blobs & secrets

#[derive(Default)]
pub struct MemBlobStore {
    m: Mutex<HashMap<BlobHash, Vec<u8>>>,
}

/// Non-cryptographic stand-in hash for the mock (real impl uses BLAKE3).
fn fake_hash(b: &[u8]) -> BlobHash {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    BlobHash(format!("mock{:016x}", h.finish()))
}

impl BlobStore for MemBlobStore {
    fn put(&self, bytes: &[u8]) -> Result<BlobHash> {
        let h = fake_hash(bytes);
        self.m.lock().unwrap().insert(h.clone(), bytes.to_vec());
        Ok(h)
    }
    fn put_file(&self, path: &std::path::Path) -> Result<BlobHash> {
        self.put(&std::fs::read(path)?)
    }
    fn get(&self, hash: &BlobHash) -> Result<Option<Vec<u8>>> {
        Ok(self.m.lock().unwrap().get(hash).cloned())
    }
    fn has(&self, hash: &BlobHash) -> Result<bool> {
        Ok(self.m.lock().unwrap().contains_key(hash))
    }
    fn path(&self, _: &BlobHash) -> Result<Option<std::path::PathBuf>> {
        Ok(None)
    }
}

#[derive(Default)]
pub struct MemSecretStore {
    m: Mutex<HashMap<String, Vec<u8>>>,
}

impl SecretStore for MemSecretStore {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.m.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &[u8]) -> Result<()> {
        self.m.lock().unwrap().insert(key.into(), value.to_vec());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<()> {
        self.m.lock().unwrap().remove(key);
        Ok(())
    }
}

// ------------------------------------------------------------------ audio

/// Emits `frames` frames of a 440 Hz tone (mic) or silence (system), then ends.
pub struct MockAudioBackend {
    pub frames: u32,
}

#[async_trait]
impl AudioBackend for MockAudioBackend {
    fn name(&self) -> &str {
        "mock"
    }
    async fn list_devices(&self) -> Result<Vec<AudioDeviceInfo>> {
        Ok(vec![
            AudioDeviceInfo {
                id: "mock-mic".into(),
                name: "Mock Microphone".into(),
                channel: AudioChannel::Mic,
                backend: "mock".into(),
                is_default: true,
            },
            AudioDeviceInfo {
                id: "mock-system".into(),
                name: "Mock Speakers (loopback)".into(),
                channel: AudioChannel::System,
                backend: "mock".into(),
                is_default: true,
            },
        ])
    }
    async fn open(
        &self,
        channel: AudioChannel,
        cfg: CaptureConfig,
    ) -> Result<Box<dyn AudioStream>> {
        Ok(Box::new(MockAudioStream {
            channel,
            frame_ms: cfg.frame_ms.max(10),
            left: self.frames,
            pos_ms: 0,
        }))
    }
}

struct MockAudioStream {
    channel: AudioChannel,
    frame_ms: u32,
    left: u32,
    pos_ms: u64,
}

#[async_trait]
impl AudioStream for MockAudioStream {
    async fn next(&mut self) -> Option<Result<AudioFrame>> {
        if self.left == 0 {
            return None;
        }
        self.left -= 1;
        let n = (PIPELINE_SAMPLE_RATE * self.frame_ms / 1000) as usize;
        let t0 = self.pos_ms;
        let samples = match self.channel {
            AudioChannel::System => vec![0.0; n],
            _ => (0..n)
                .map(|i| {
                    let t = (t0 as f32 / 1000.0) + i as f32 / PIPELINE_SAMPLE_RATE as f32;
                    0.2 * (2.0 * std::f32::consts::PI * 440.0 * t).sin()
                })
                .collect(),
        };
        self.pos_ms += self.frame_ms as u64;
        Some(Ok(AudioFrame {
            channel: self.channel,
            t0_ms: t0,
            samples,
        }))
    }
    async fn stop(&mut self) -> Result<()> {
        self.left = 0;
        Ok(())
    }
}

// ------------------------------------------------------------------ stt

/// Returns one canned segment per call / per 3 s of streamed audio.
pub struct MockStt {
    pub text: String,
    pub lang: LangTag,
}

impl Default for MockStt {
    fn default() -> Self {
        MockStt {
            text: "Xin chào, hôm nay chúng ta họp về kế hoạch Q4.".into(),
            lang: LangTag::vi(),
        }
    }
}

fn seg(idx: u32, t0: u64, t1: u64, ch: AudioChannel, text: &str, lang: &LangTag) -> Segment {
    Segment {
        idx,
        t0_ms: t0 as u32,
        t1_ms: t1 as u32,
        channel: ch,
        speaker: None,
        lang: Some(lang.clone()),
        text: text.into(),
        confidence: Some(0.9),
        words: None,
    }
}

#[async_trait]
impl SttEngine for MockStt {
    fn model(&self) -> SttModelInfo {
        SttModelInfo {
            id: "mock:stt".into(),
            engine: "mock".into(),
            languages: vec![LangTag::vi(), LangTag::en()],
            streaming: true,
            size_bytes: None,
            sha256: None,
            installed: true,
        }
    }
    async fn transcribe(
        &self,
        samples: &[f32],
        t_offset_ms: u64,
        channel: AudioChannel,
        _: &SttOptions,
    ) -> Result<Vec<Segment>> {
        let dur = samples.len() as u64 * 1000 / PIPELINE_SAMPLE_RATE as u64;
        Ok(vec![seg(
            0,
            t_offset_ms,
            t_offset_ms + dur,
            channel,
            &self.text,
            &self.lang,
        )])
    }
    fn start_stream(&self, _: SttOptions) -> Result<Box<dyn SttStream>> {
        Ok(Box::new(MockSttStream {
            text: self.text.clone(),
            lang: self.lang.clone(),
            buffered_ms: 0,
            start_ms: None,
            idx: 0,
            channel: AudioChannel::Mic,
        }))
    }
}

struct MockSttStream {
    text: String,
    lang: LangTag,
    buffered_ms: u64,
    start_ms: Option<u64>,
    idx: u32,
    channel: AudioChannel,
}

#[async_trait]
impl SttStream for MockSttStream {
    async fn push(&mut self, f: AudioFrame) -> Result<Vec<SttUpdate>> {
        self.channel = f.channel;
        let start = *self.start_ms.get_or_insert(f.t0_ms);
        self.buffered_ms += f.samples.len() as u64 * 1000 / PIPELINE_SAMPLE_RATE as u64;
        let s = seg(
            self.idx,
            start,
            start + self.buffered_ms,
            f.channel,
            &self.text,
            &self.lang,
        );
        if self.buffered_ms >= 3000 {
            self.idx += 1;
            self.buffered_ms = 0;
            self.start_ms = None;
            Ok(vec![SttUpdate {
                segment: s,
                is_final: true,
            }])
        } else {
            Ok(vec![SttUpdate {
                segment: s,
                is_final: false,
            }])
        }
    }
    async fn finish(&mut self) -> Result<Vec<SttUpdate>> {
        if self.buffered_ms == 0 {
            return Ok(vec![]);
        }
        let start = self.start_ms.unwrap_or(0);
        Ok(vec![SttUpdate {
            segment: seg(
                self.idx,
                start,
                start + self.buffered_ms,
                self.channel,
                &self.text,
                &self.lang,
            ),
            is_final: true,
        }])
    }
}

// ------------------------------------------------------------------ llm

/// Returns `reply` (or, when a JSON schema is requested, `json_reply`) verbatim.
pub struct MockLlm {
    pub reply: String,
    pub json_reply: serde_json::Value,
}

impl Default for MockLlm {
    fn default() -> Self {
        MockLlm {
            reply: "Tóm tắt: nhóm thống nhất kế hoạch Q4.".into(),
            json_reply: serde_json::json!({}),
        }
    }
}

#[async_trait]
impl LlmProvider for MockLlm {
    fn name(&self) -> &str {
        "mock"
    }
    async fn health(&self) -> Result<()> {
        Ok(())
    }
    async fn list_models(&self) -> Result<Vec<LlmModelInfo>> {
        Ok(vec![LlmModelInfo {
            name: "mock:latest".into(),
            digest: Some("mock".into()),
            size_bytes: None,
            context_length: Some(32768),
        }])
    }
    async fn chat(&self, req: ChatRequest) -> Result<ChatResponse> {
        let content = if req.json_schema.is_some() {
            self.json_reply.to_string()
        } else {
            self.reply.clone()
        };
        Ok(ChatResponse {
            content,
            model: self.list_models().await?.remove(0),
            prompt_tokens: None,
            completion_tokens: None,
        })
    }
    async fn chat_stream(&self, req: ChatRequest) -> Result<BoxStream<'static, Result<String>>> {
        let full = self.chat(req).await?.content;
        let parts: Vec<Result<String>> = full
            .split_inclusive(' ')
            .map(|s| Ok(s.to_string()))
            .collect();
        Ok(stream::iter(parts).boxed())
    }
}

// ------------------------------------------------------------------ calendar

pub struct MockCalendar {
    pub id: String,
    pub events: Vec<CalendarEvent>,
}

impl MockCalendar {
    /// One Google-Meet event starting `in_minutes` from `now`.
    pub fn with_upcoming(id: &str, now: UnixMs, in_minutes: i64) -> Self {
        let start = now + in_minutes * 60_000;
        MockCalendar {
            id: id.into(),
            events: vec![CalendarEvent {
                source: id.into(),
                event_id: "evt1".into(),
                calendar_id: "primary".into(),
                calendar_name: Some("Work".into()),
                title: "Họp kế hoạch Q4 / Q4 planning".into(),
                description: Some("Agenda: budget, hiring".into()),
                location: None,
                start,
                end: start + 30 * 60_000,
                all_day: false,
                attendees: vec![],
                conference_url: Some("https://meet.google.com/abc-defg-hij".into()),
                conference_app: Some("google_meet".into()),
                organizer: None,
                status: "confirmed".into(),
                updated: now,
            }],
        }
    }
}

#[async_trait]
impl CalendarSource for MockCalendar {
    fn info(&self) -> CalendarSourceInfo {
        CalendarSourceInfo {
            id: self.id.clone(),
            provider: "mock".into(),
            label: self.id.clone(),
            enabled: true,
        }
    }
    async fn sync(&self, cursor: Option<String>) -> Result<CalendarDelta> {
        Ok(CalendarDelta {
            upserts: if cursor.is_none() {
                self.events.clone()
            } else {
                vec![]
            },
            deletes: vec![],
            next_cursor: Some("mock-cursor".into()),
        })
    }
}

// ------------------------------------------------------------------ verify / pipeline

/// Marks claims with evidence as Supported, without as Unverifiable.
pub struct MockVerifier;

#[async_trait]
impl Verifier for MockVerifier {
    fn descriptor(&self) -> VerifierDescriptor {
        VerifierDescriptor {
            name: "verify.mock".into(),
            version: "0".into(),
            family: "symbolic".into(),
            logical_forms: vec![],
            description: "Supported iff the claim cites evidence".into(),
        }
    }
    async fn verify(&self, claims: &[Claim], ctx: &VerifyContext) -> Result<Vec<Verdict>> {
        Ok(claims
            .iter()
            .map(|c| Verdict {
                claims_artifact: ctx.claims_artifact.id.clone(),
                claim_id: c.id.clone(),
                verifier: "verify.mock".into(),
                label: if c.evidence.is_empty() {
                    VerdictLabel::Unverifiable
                } else {
                    VerdictLabel::Supported
                },
                confidence: 0.5,
                evidence: c.evidence.clone(),
                explanation: "mock".into(),
                trace: None,
            })
            .collect())
    }
}

/// A processor that returns the given drafts (with `meeting_id` rewritten to the job's meeting).
pub struct MockProcessor {
    pub desc: ProcessorDescriptor,
    pub output: Vec<ArtifactDraft>,
}

#[async_trait]
impl Processor for MockProcessor {
    fn descriptor(&self) -> ProcessorDescriptor {
        self.desc.clone()
    }
    async fn run(&self, ctx: &JobCtx, _inputs: Vec<Artifact>) -> Result<Vec<ArtifactDraft>> {
        ctx.check_cancel()?;
        (ctx.progress)(1.0, "done");
        Ok(self
            .output
            .iter()
            .cloned()
            .map(|mut d| {
                d.meeting_id = ctx.meeting.id.clone();
                d
            })
            .collect())
    }
}

// ------------------------------------------------------------------ transport

type Pipe = (
    tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
    tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
);

/// A shared in-process "LAN". Create one, then `MemNetwork::transport(device)` per device.
#[derive(Clone, Default)]
pub struct MemNetwork {
    inner: Arc<Mutex<NetInner>>,
}

#[derive(Default)]
struct NetInner {
    devices: BTreeMap<DeviceId, DeviceInfo>,
    inboxes: HashMap<DeviceId, tokio::sync::mpsc::UnboundedSender<MemConnection>>,
}

impl MemNetwork {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn transport(&self, info: DeviceInfo) -> MemTransport {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut g = self.inner.lock().unwrap();
        g.devices.insert(info.id.clone(), info.clone());
        g.inboxes.insert(info.id.clone(), tx);
        MemTransport {
            me: info.id,
            net: self.clone(),
            inbox: tokio::sync::Mutex::new(rx),
        }
    }
}

pub struct MemTransport {
    me: DeviceId,
    net: MemNetwork,
    inbox: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<MemConnection>>,
}

pub struct MemConnection {
    peer: DeviceId,
    pipe: Pipe,
}

#[async_trait]
impl Connection for MemConnection {
    fn peer(&self) -> &DeviceId {
        &self.peer
    }
    async fn send(&mut self, msg: Vec<u8>) -> Result<()> {
        self.pipe
            .0
            .send(msg)
            .map_err(|_| Error::Network("closed".into()))
    }
    async fn recv(&mut self) -> Result<Option<Vec<u8>>> {
        Ok(self.pipe.1.recv().await)
    }
    async fn close(&mut self) -> Result<()> {
        self.pipe.1.close();
        Ok(())
    }
}

#[async_trait]
impl Transport for MemTransport {
    fn local_device(&self) -> DeviceId {
        self.me.clone()
    }
    async fn discover(&self) -> Result<Vec<PeerInfo>> {
        let g = self.net.inner.lock().unwrap();
        Ok(g.devices
            .values()
            .filter(|d| d.id != self.me)
            .map(|d| PeerInfo {
                device: d.clone(),
                addrs: vec!["mem".into()],
                paired: true,
                last_seen: Some(crate::time::now_ms()),
                last_synced: None,
            })
            .collect())
    }
    async fn add_addr(&self, _: Option<DeviceId>, _: String) -> Result<()> {
        Ok(())
    }
    async fn connect(&self, peer: &DeviceId) -> Result<Box<dyn Connection>> {
        let (a_tx, a_rx) = tokio::sync::mpsc::unbounded_channel();
        let (b_tx, b_rx) = tokio::sync::mpsc::unbounded_channel();
        let inbox = self
            .net
            .inner
            .lock()
            .unwrap()
            .inboxes
            .get(peer)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("peer {peer}")))?;
        inbox
            .send(MemConnection {
                peer: self.me.clone(),
                pipe: (b_tx, a_rx),
            })
            .map_err(|_| Error::Network("peer offline".into()))?;
        Ok(Box::new(MemConnection {
            peer: peer.clone(),
            pipe: (a_tx, b_rx),
        }))
    }
    async fn accept(&self) -> Result<Box<dyn Connection>> {
        self.inbox
            .lock()
            .await
            .recv()
            .await
            .map(|c| Box::new(c) as Box<dyn Connection>)
            .ok_or_else(|| Error::Network("network dropped".into()))
    }
}

// ------------------------------------------------------------------ fixtures

pub mod fixtures {
    //! Sample data shared by tests and the UI mock.
    use super::*;
    use crate::artifact::body::*;
    use crate::artifact::{ArtifactKind, Provenance, SpanRef};
    use crate::meeting::MeetingKind;

    pub fn device(name: &str) -> DeviceInfo {
        DeviceInfo {
            id: DeviceId(format!("dev-{name}")),
            name: name.into(),
            platform: "windows".into(),
            app_version: "0.1.0".into(),
            capabilities: vec![],
        }
    }

    pub fn meeting(now: UnixMs) -> Meeting {
        Meeting {
            id: Id::new(),
            title: "Họp kế hoạch Q4".into(),
            started_at: now,
            ended_at: Some(now + 30 * 60_000),
            kind: MeetingKind::Online {
                app: Some("google_meet".into()),
            },
            calendar_event: None,
            languages: vec![LangTag::vi(), LangTag::en()],
            audio: vec![],
            notes_doc: Id::new(),
            tags: vec![],
        }
    }

    /// A short bilingual transcript.
    pub fn transcript(meeting: &Id, device: &DeviceId) -> Artifact {
        let lines = [
            (
                "me",
                "vi",
                "Chào mọi người, hôm nay mình bàn về ngân sách Q4.",
            ),
            ("S1", "vi", "Ngân sách marketing là 200 triệu đồng."),
            (
                "S2",
                "en",
                "We should hire two backend engineers by December.",
            ),
            ("me", "vi", "Ok, Lan sẽ gửi bản kế hoạch trước thứ Sáu."),
        ];
        let segments = lines
            .iter()
            .enumerate()
            .map(|(i, (spk, lang, text))| Segment {
                idx: i as u32,
                t0_ms: i as u32 * 5000,
                t1_ms: i as u32 * 5000 + 4500,
                channel: if *spk == "me" {
                    AudioChannel::Mic
                } else {
                    AudioChannel::System
                },
                speaker: Some(crate::ids::SpeakerId(spk.to_string())),
                lang: Some(LangTag(lang.to_string())),
                text: text.to_string(),
                confidence: Some(0.92),
                words: None,
            })
            .collect();
        let body = Transcript {
            segments,
            speakers: vec![],
        };
        ArtifactDraft::new(
            meeting.clone(),
            &body,
            Provenance {
                processor: "transcribe.mock".into(),
                processor_version: "0".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .into_artifact(Id::new(), crate::time::Hlc::default(), device.clone())
    }

    /// Summary of [`transcript`] with citations (one bullet deliberately wrong: 300 vs 200).
    pub fn summary(transcript: &Artifact, device: &DeviceId) -> Artifact {
        let cite = |i: u32| SpanRef {
            artifact: transcript.id.clone(),
            segment: i,
            chars: None,
            time_ms: None,
        };
        let body = Summary {
            lang: Some(LangTag::vi()),
            sections: vec![SummarySection {
                heading: "Quyết định".into(),
                bullets: vec![
                    CitedText {
                        text: "Ngân sách marketing Q4 là 300 triệu đồng.".into(),
                        cites: vec![cite(1)],
                    },
                    CitedText {
                        text: "Tuyển 2 kỹ sư backend trước tháng 12.".into(),
                        cites: vec![cite(2)],
                    },
                    CitedText {
                        text: "Lan gửi kế hoạch trước thứ Sáu.".into(),
                        cites: vec![cite(3)],
                    },
                ],
            }],
        };
        let mut d = ArtifactDraft::new(
            transcript.meeting_id.clone(),
            &body,
            Provenance {
                processor: "summarize.mock".into(),
                processor_version: "0".into(),
                inputs: vec![transcript.id.clone()],
                ..Default::default()
            },
        )
        .unwrap();
        d.kind = ArtifactKind::new(ArtifactKind::SUMMARY);
        d.into_artifact(Id::new(), crate::time::Hlc::default(), device.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn two_stores_converge_over_mem_network() {
        let a = MemStore::new(DeviceId("a".into()));
        let b = MemStore::new(DeviceId("b".into()));
        let m = fixtures::meeting(1_000);
        a.append_local(EventBody::MeetingUpsert(m.clone())).unwrap();
        let t = fixtures::transcript(&m.id, &a.device_id());
        a.append_local(EventBody::ArtifactAdd(t)).unwrap();

        let missing = a.events_missing(&b.version_vector().unwrap(), 100).unwrap();
        assert_eq!(b.apply_remote(missing.clone()).unwrap(), 2);
        assert_eq!(b.apply_remote(missing).unwrap(), 0, "idempotent");
        assert!(b.get_meeting(&m.id).unwrap().is_some());
        assert_eq!(
            b.list_artifacts(&m.id, Some("transcript")).unwrap().len(),
            1
        );
        assert!(
            b.version_vector()
                .unwrap()
                .dominates(&a.version_vector().unwrap())
        );
    }

    #[tokio::test]
    async fn mem_transport_roundtrip() {
        let net = MemNetwork::new();
        let ta = net.transport(fixtures::device("a"));
        let tb = net.transport(fixtures::device("b"));
        let mut ca = ta.connect(&tb.local_device()).await.unwrap();
        let mut cb = tb.accept().await.unwrap();
        ca.send(b"hi".to_vec()).await.unwrap();
        assert_eq!(cb.recv().await.unwrap().unwrap(), b"hi");
        assert_eq!(cb.peer(), &ta.local_device());
    }

    #[test]
    fn typed_artifact_decode() {
        let d = DeviceId("a".into());
        let m = fixtures::meeting(0);
        let t = fixtures::transcript(&m.id, &d);
        let body: crate::artifact::body::Transcript = t.decode().unwrap();
        assert_eq!(body.segments.len(), 4);
        assert!(t.decode::<crate::artifact::body::Summary>().is_err());
    }

    #[test]
    fn unknown_event_kind_is_kept() {
        let s = MemStore::new(DeviceId("a".into()));
        let ev = Event {
            device: DeviceId("z".into()),
            seq: 1,
            hlc: Default::default(),
            kind: EventKind("future.thing".into()),
            body: serde_json::json!({"x": 1}),
        };
        assert_eq!(s.apply_remote(vec![ev]).unwrap(), 1);
        assert_eq!(
            s.events_missing(&VersionVector::default(), 10)
                .unwrap()
                .len(),
            1
        );
    }
}
