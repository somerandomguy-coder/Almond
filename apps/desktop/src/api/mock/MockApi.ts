// In-memory implementation of AlmondApi. Realistic enough to click through every screen:
// live captions while recording, job progress after Stop, on-demand sync with two fake peers,
// pairing, calendar reminders, actions.
import type { Artifact } from "../../gen/Artifact";
import type { CalendarEvent } from "../../gen/CalendarEvent";
import type { CalendarSourceInfo } from "../../gen/CalendarSourceInfo";
import type { Job } from "../../gen/Job";
import type { JobProgress } from "../../gen/JobProgress";
import type { Meeting } from "../../gen/Meeting";
import type { MeetingDetail } from "../../gen/MeetingDetail";
import type { ModelEntry } from "../../gen/ModelEntry";
import type { ModelSlot } from "../../gen/ModelSlot";
import type { PeerInfo } from "../../gen/PeerInfo";
import type { RecordingStatus } from "../../gen/RecordingStatus";
import type { SearchHit } from "../../gen/SearchHit";
import type { Segment } from "../../gen/Segment";
import type { SummarySection } from "../../gen/SummarySection";
import type { SyncReport } from "../../gen/SyncReport";
import type { Transcript } from "../../gen/Transcript";
import type { JsonValue } from "../../gen/serde_json/JsonValue";
import { ulid } from "../../lib/ids";
import { fold, snippet } from "../../lib/text";
import type { AlmondApi, ApiNotifications, Method, NotificationName, Params, Result, Unlisten } from "../types";
import { ApiError } from "../errors";
import * as fx from "./fixtures";
import { builtinRecipes, mockRecipeOutput } from "./recipes";
import type { RecipeInfo } from "../types";
import type { Transcript as TranscriptBody } from "../../gen/Transcript";

export interface MockOptions {
  /** Time multiplier: 1 = realistic pacing, 20 = tests. */
  speed?: number;
  /** Fire a `calendar.reminder` this many ms after start (null = never). */
  reminderAfterMs?: number | null;
  /** Platform to pretend to be ("windows" | "linux" | "android"). */
  platform?: string;
  now?: number;
}

type Handler = (payload: unknown) => void;

const IDLE: RecordingStatus = { state: "idle", meeting_id: null, started_at: null, elapsed_ms: 0, levels: [] };

export class MockApi implements AlmondApi {
  readonly kind = "mock" as const;
  private speed: number;
  private plat: string;
  private listeners = new Map<string, Set<Handler>>();
  private timers = new Set<ReturnType<typeof setTimeout>>();

  meetings: Meeting[];
  artifacts: Artifact[];
  private notes = new Map<string, string[]>();
  private events: CalendarEvent[];
  private sources: CalendarSourceInfo[];
  private peers: PeerInfo[];
  private models: ModelEntry[];
  private settings = new Map<string, JsonValue>();
  private jobs = new Map<string, JobProgress>();
  private lastReports: SyncReport[] = [];
  private syncing = false;
  private phoneDelivered = false;
  private rec: RecordingStatus = { ...IDLE };
  private recTimer: ReturnType<typeof setInterval> | null = null;
  private liveSegments: Segment[] = [];
  private importCancels = new Set<string>();
  /** Cookbook (recipes.*): built-ins first, then user recipes. */
  recipes: RecipeInfo[] = builtinRecipes();
  /** What the mock "file picker" returns (`null` = the user closed it). Tests may change it. */
  pickedFile: string | null = "C:\\Users\\me\\Recordings\\Phỏng vấn khách hàng.m4a";

  /** Stand-in for the native file dialog (see lib/importMedia.ts). */
  pickFile(): Promise<string | null> {
    return Promise.resolve(this.pickedFile);
  }

  constructor(opts: MockOptions = {}) {
    this.speed = opts.speed ?? 1;
    this.plat = opts.platform ?? "windows";
    const now = opts.now ?? Date.now();
    const s = fx.seed(now);
    this.meetings = s.meetings;
    this.artifacts = s.artifacts;
    this.events = fx.calendarEvents(now);
    this.sources = fx.calendarSources();
    this.peers = fx.peers(now);
    this.models = fx.models();
    this.settings.set("models.slots", fx.modelSlots() as unknown as JsonValue);
    this.settings.set("calendar.reminder_lead_min", 5);
    this.settings.set("sync.auto_while_open", false);
    // Link the 3rd upcoming event to nothing, the Q4 meeting to a past event (not shown upcoming).
    const reminderAfter = opts.reminderAfterMs === undefined ? 8000 : opts.reminderAfterMs;
    if (reminderAfter != null) {
      this.after(reminderAfter, () => this.fireReminder());
    }
  }

  // ---- plumbing ------------------------------------------------------------------------------

  platform(): Promise<string> {
    return Promise.resolve(this.plat);
  }

  on<N extends NotificationName>(name: N, cb: (payload: ApiNotifications[N]) => void): Unlisten {
    let set = this.listeners.get(name);
    if (!set) this.listeners.set(name, (set = new Set()));
    set.add(cb as Handler);
    return () => set!.delete(cb as Handler);
  }

  emit<N extends NotificationName>(name: N, payload: ApiNotifications[N]) {
    this.listeners.get(name)?.forEach((h) => h(payload));
  }

  /** Stop all timers (tests). */
  dispose() {
    this.timers.forEach((t) => clearTimeout(t));
    this.timers.clear();
    if (this.recTimer) clearInterval(this.recTimer);
  }

  private after(ms: number, fn: () => void) {
    const t = setTimeout(() => {
      this.timers.delete(t);
      fn();
    }, ms / this.speed);
    this.timers.add(t);
  }

  private sleep(ms: number) {
    return new Promise<void>((r) => this.after(ms, r));
  }

  fireReminder(event?: CalendarEvent) {
    const ev = event ?? this.events[0];
    const lead = Number(this.settings.get("calendar.reminder_lead_min") ?? 5);
    this.emit("calendar.reminder", { event: ev, minutes_before: lead });
  }

  async call<M extends Method>(method: M, params: Params<M>): Promise<Result<M>> {
    await this.sleep(40);
    const fn = (this.handlers as Record<string, (p: unknown) => unknown>)[method];
    if (!fn) throw new ApiError(-32601, `mock: method not implemented: ${method}`);
    return (await fn.call(this, params)) as Result<M>;
  }

  // ---- helpers -------------------------------------------------------------------------------

  private meeting(id: string): Meeting {
    const m = this.meetings.find((x) => x.id === id);
    if (!m) throw new ApiError(-32004, `meeting not found: ${id}`);
    return m;
  }

  private hlc() {
    return { wall_ms: Date.now(), logical: 0 };
  }

  private artifactsFor(meeting_id: string, kind?: string): Artifact[] {
    const all = this.artifacts.filter((a) => a.meeting_id === meeting_id && (!kind || a.kind === kind));
    const superseded = new Set(all.map((a) => a.supersedes).filter(Boolean));
    const byTime = (a: Artifact, b: Artifact) => b.created_at.wall_ms - a.created_at.wall_ms || b.created_at.logical - a.created_at.logical;
    const latest = all.filter((a) => !superseded.has(a.id)).sort(byTime);
    const older = all.filter((a) => superseded.has(a.id)).sort(byTime);
    return [...latest, ...older];
  }

  private addArtifact(a: Artifact) {
    this.artifacts.push(a);
    this.emit("artifact.added", a);
  }

  private newJob(meeting_id: string, processor: string, inputs: string[], params: JsonValue = {}, requires: string[] = []): Job {
    return {
      id: ulid(),
      meeting_id,
      processor,
      inputs,
      params,
      requires,
      requested_by: fx.thisDevice.id,
      requested_at: this.hlc(),
      priority: 0,
    };
  }

  /** Simulate a job: pending → claimed (progress steps) → done, then run `produce`. */
  private async runJob(job: Job, message: string, durationMs: number, produce: () => void | Promise<void>) {
    const jp: JobProgress = { job, status: "pending", device: null, progress: 0, message };
    this.jobs.set(job.id, jp);
    this.emit("job.progress", { ...jp });
    await this.sleep(300);
    const steps = 5;
    for (let i = 1; i <= steps; i++) {
      if (this.jobs.get(job.id)?.status === "cancelled") return;
      const upd: JobProgress = { job, status: "claimed", device: fx.thisDevice.id, progress: i / (steps + 1), message };
      this.jobs.set(job.id, upd);
      this.emit("job.progress", upd);
      await this.sleep(durationMs / steps);
    }
    await produce();
    const done: JobProgress = { job, status: "done", device: fx.thisDevice.id, progress: 1, message };
    this.jobs.set(job.id, done);
    this.emit("job.progress", done);
  }

  // ---- recording -----------------------------------------------------------------------------

  private startLive(meeting_id: string) {
    this.liveSegments = [];
    const started = Date.now();
    const LINE_MS = 2600; // one caption line per 2.6 s of "audio": partial at 1.4 s, final at 2.6 s
    let emitted = 0; // number of (partial|final) events emitted so far: 2 per line
    // Real-time tick stays >= 100 ms even when the mock is sped up, so tests don't re-render at 100 Hz.
    this.recTimer = setInterval(() => {
      const elapsed = (Date.now() - started) * this.speed;
      const wobble = (k: number) => Math.max(0.02, Math.min(1, 0.35 + 0.3 * Math.sin(elapsed / (300 + k * 170)) + Math.random() * 0.2));
      this.rec = { ...this.rec, elapsed_ms: elapsed, levels: [["mic", wobble(1)], ["system", wobble(2)]] };
      this.emit("recording.status", this.rec);
      const maxEvents = fx.LIVE_LINES.length * 3 * 2;
      while (emitted < maxEvents) {
        const lineIdx = Math.floor(emitted / 2);
        const final = emitted % 2 === 1;
        const due = lineIdx * LINE_MS + (final ? LINE_MS : 1400);
        if (elapsed < due) break;
        // Skip stale partials when catching up; only the final matters then.
        if (!final && elapsed >= (lineIdx + 1) * LINE_MS) {
          emitted++;
          continue;
        }
        this.emitSegment(meeting_id, lineIdx, fx.LIVE_LINES[lineIdx % fx.LIVE_LINES.length], due, final);
        emitted++;
      }
    }, Math.max(100, 250 / this.speed));
  }

  private emitSegment(meeting_id: string, idx: number, line: [string, string, string], elapsed: number, final: boolean) {
    const [spk, lang, text] = line;
    const words = text.split(" ");
    const seg: Segment = {
      idx,
      t0_ms: Math.max(0, elapsed - 2600),
      t1_ms: elapsed,
      channel: spk === "me" ? "mic" : "system",
      speaker: spk,
      lang,
      text: final ? text : words.slice(0, Math.max(2, Math.ceil(words.length * 0.55))).join(" "),
      confidence: final ? 0.9 : 0.6,
      words: null,
    };
    if (final) this.liveSegments.push(seg);
    this.emit("transcript.update", { meeting_id, update: { segment: seg, is_final: final } });
  }

  private async finishRecording(meeting: Meeting) {
    const segs = this.liveSegments.length ? this.liveSegments : fx.segmentsFrom(fx.LIVE_LINES.slice(0, 3));
    const ctx = { meeting_id: meeting.id, device: fx.thisDevice.id, wall: Date.now() };
    const transcribe = this.newJob(meeting.id, "transcribe.whisper", [], {}, ["gpu", "stt-large"]);
    let transcript!: Artifact;
    await this.runJob(transcribe, "PhoWhisper-large · GPU", 2500, () => {
      transcript = fx.transcriptArtifact(ctx, segs.map((s, i) => ({ ...s, idx: i })));
      this.addArtifact(transcript);
    });
    const n = segs.length;
    const pick = (want: number) => Math.min(want, n - 1);
    const sections: Parameters<typeof fx.summaryBundle>[2] = [
      [
        "Tiến độ",
        [
          ["Pairing bằng mã QR đã xong; tiếp theo là sync blob.", [pick(1)]],
          ["Lỗi WASAPI loopback trên Windows đã được sửa.", [pick(2)]],
        ],
      ],
      [
        "Quyết định",
        [
          ["Dời ngày phát hành sang thứ Tư 18/11.", [pick(5), pick(6)]],
          ["Hướng dẫn người dùng tắt tối ưu hoá pin trên Galaxy A26.", [pick(4)], ["needs_review", 0.6, "Suggested, not explicitly agreed."]],
        ],
      ],
      ["Việc cần làm", [["Lan gửi lịch mới trong hôm nay.", [pick(7)]]]],
    ];
    const summarize = this.newJob(meeting.id, "summarize.llm", [transcript.id], {}, ["llm"]);
    let bundle: Artifact[] = [];
    await this.runJob(summarize, "qwen3.5:9b", 2500, () => {
      bundle = fx.summaryBundle({ ...ctx, wall: Date.now() }, transcript, sections, { model: "qwen3.5:9b" });
      this.addArtifact(bundle[0]);
    });
    const verify = this.newJob(meeting.id, "verify.run", [bundle[0].id], {}, ["llm"]);
    await this.runJob(verify, "claims · NLI · numbers", 1500, () => {
      this.addArtifact(bundle[1]);
      this.addArtifact(bundle[2]);
    });
    const items = this.newJob(meeting.id, "action_items.llm", [transcript.id], {}, ["llm"]);
    await this.runJob(items, "qwen3.5:9b", 1000, () => {
      const c = (i: number) => {
        const s = transcript.body as unknown as Transcript;
        const seg = s.segments[pick(i)];
        return { artifact: transcript.id, segment: seg.idx, chars: null, time_ms: [seg.t0_ms, seg.t1_ms] as [number, number] };
      };
      this.addArtifact(
        fx.makeArtifact({ ...ctx, wall: Date.now() }, "action_items", { items: [{ text: "Gửi lịch phát hành mới", owner: "Trần Lan", due: "today", cites: [c(7)] }] }, { processor: "action_items.llm", model: "ollama:qwen3.5:9b", inputs: [transcript.id] }),
      );
      this.addArtifact(
        fx.makeArtifact(
          { ...ctx, wall: Date.now() + 1 },
          "action_proposals",
          {
            proposals: [
              { id: ulid(), action: "gmail.draft_follow_up", title: "Draft follow-up email to attendees", preview: "Chào mọi người,\n\nPhát hành dời sang thứ Tư 18/11. Lan sẽ gửi lịch mới hôm nay.\n\nNam", payload: { subject: `Follow-up: ${meeting.title}` } },
              { id: ulid(), action: "export.markdown", title: "Export notes as Markdown", preview: `# ${meeting.title}\n…`, payload: {} },
            ],
          },
          { processor: "actions.propose", inputs: [transcript.id] },
        ),
      );
    });
  }

  // ---- method table --------------------------------------------------------------------------

  private handlers: { [M in Method]: (this: MockApi, p: Params<M>) => Result<M> | Promise<Result<M>> } = {
    "meetings.list": function (q) {
      let list = [...this.meetings].sort((a, b) => b.started_at - a.started_at);
      if (q.text) {
        const t = fold(q.text);
        list = list.filter((m) => fold(m.title).includes(t));
      }
      if (q.from != null) list = list.filter((m) => m.started_at >= q.from!);
      if (q.to != null) list = list.filter((m) => m.started_at <= q.to!);
      if (q.tag) list = list.filter((m) => m.tags.includes(q.tag!));
      const off = q.offset ?? 0;
      return list.slice(off, q.limit != null ? off + q.limit : undefined);
    },
    "meetings.get": function ({ id }): MeetingDetail {
      const meeting = this.meeting(id);
      const ev = meeting.calendar_event ? this.events.find((e) => e.event_id === meeting.calendar_event!.event_id) ?? null : null;
      return { meeting, artifacts: this.artifactsFor(id), calendar_event: ev };
    },
    "meetings.create": function (p) {
      const now = Date.now();
      const ev = p.calendar_event ? this.events.find((e) => e.event_id === p.calendar_event!.event_id) : undefined;
      const m: Meeting = {
        id: ulid(now),
        title: p.title ?? ev?.title ?? "",
        tz: Intl.DateTimeFormat().resolvedOptions().timeZone ?? null,
        started_at: now,
        ended_at: null,
        kind: p.kind,
        calendar_event: p.calendar_event,
        languages: ["vi", "en"],
        audio: [],
        notes_doc: ulid(now + 1),
        tags: [],
      };
      this.meetings.push(m);
      return m;
    },
    "meetings.update": function (p) {
      const m = this.meeting(p.id);
      if (p.title != null) m.title = p.title;
      if (p.tags != null) m.tags = p.tags;
      return { ...m };
    },
    "meetings.delete": function ({ id }) {
      this.meetings = this.meetings.filter((m) => m.id !== id);
      this.artifacts = this.artifacts.filter((a) => a.meeting_id !== id);
      return null;
    },
    "meetings.import": async function (p) {
      const path = p.path.trim();
      const fileName = path.split(/[\\/]/).pop() ?? path;
      if (!fileName) throw new ApiError(-32602, "path is empty");
      if (/\.(txt|pdf|docx?)$/i.test(fileName)) throw new ApiError(-32005, `cannot import ${fileName}: not an audio or video file`);
      const now = Date.now();
      const id = ulid(now);
      const duration = 23 * 60_000 + 41_000;
      const base = { meeting_id: id, path, file_name: fileName, duration_ms: duration, decoder: "symphonia", error: null };
      const steps = 8;
      for (let i = 0; i <= steps; i++) {
        if (this.importCancels.delete(id)) {
          this.emit("import.progress", { ...base, stage: "cancelled", progress: null, processed_ms: 0 });
          throw new ApiError(-32603, "cancelled");
        }
        this.emit("import.progress", { ...base, stage: "decoding", progress: i / steps, processed_ms: Math.round((duration * i) / steps) });
        await this.sleep(150);
      }
      this.emit("import.progress", { ...base, stage: "storing", progress: 1, processed_ms: duration });
      const m: Meeting = {
        id,
        title: p.title?.trim() || fileName.replace(/\.[^.]+$/, ""),
        tz: Intl.DateTimeFormat().resolvedOptions().timeZone ?? null,
        started_at: now - duration,
        ended_at: now,
        kind: { type: "imported" },
        calendar_event: null,
        languages: p.language ? [p.language] : [],
        audio: [{ channel: "mixed", blob: ulid(now + 2), codec: "opus", sample_rate: 16000, offset_ms: 0, duration_ms: duration }],
        notes_doc: ulid(now + 1),
        tags: [],
      };
      this.meetings.push(m);
      this.emit("import.progress", { ...base, stage: "done", progress: 1, processed_ms: duration });
      void this.finishRecording(m);
      return { ...m };
    },
    "meetings.importCancel": function ({ meeting_id }) {
      this.importCancels.add(meeting_id);
      return true;
    },
    "recording.start": async function (p) {
      if (this.rec.state !== "idle") throw new ApiError(-32000, "already recording");
      const m = p.meeting_id ? this.meeting(p.meeting_id) : await this.handlers["meetings.create"].call(this, p.create ?? { title: null, kind: { type: "online", app: null }, calendar_event: null });
      m.started_at = Date.now();
      m.ended_at = null;
      this.rec = { state: "recording", meeting_id: m.id, started_at: m.started_at, elapsed_ms: 0, levels: [] };
      if (p.live_transcript) this.startLive(m.id);
      this.emit("recording.status", this.rec);
      return this.rec;
    },
    "recording.stop": function () {
      if (this.recTimer) clearInterval(this.recTimer);
      this.recTimer = null;
      const id = this.rec.meeting_id;
      this.rec = { ...IDLE };
      this.emit("recording.status", this.rec);
      if (id) {
        const m = this.meeting(id);
        m.ended_at = Date.now();
        void this.finishRecording(m);
      }
      return this.rec;
    },
    "recording.status": function () {
      return this.rec;
    },
    "audio.devices": function () {
      return [
        { id: "mic-default", name: "Microphone (Realtek Audio)", channel: "mic", backend: "cpal", is_default: true },
        { id: "mic-usb", name: "Blue Yeti", channel: "mic", backend: "cpal", is_default: false },
        { id: "loopback-default", name: "Speakers (loopback)", channel: "system", backend: "wasapi_loopback", is_default: true },
      ];
    },
    "audio.url": function () {
      // The mock has no audio files; the player shows position only.
      return { url: null };
    },
    "artifacts.list": function ({ meeting_id, kind }) {
      return this.artifactsFor(meeting_id, kind);
    },
    "artifacts.get": function ({ id }) {
      const a = this.artifacts.find((x) => x.id === id);
      if (!a) throw new ApiError(-32004, `artifact not found: ${id}`);
      return a;
    },
    "artifacts.rerun": function (p) {
      const job = this.newJob(p.meeting_id, p.processor, p.inputs, p.params, ["llm"]);
      const transcript = this.artifactsFor(p.meeting_id, "transcript")[0];
      void this.runJob(job, p.processor.startsWith("enhance") ? "Enhancing notes · qwen3.5:9b" : `${p.processor} · qwen3.5:9b`, 2500, () => {
        if (!transcript) return;
        const ctx = { meeting_id: p.meeting_id, device: fx.thisDevice.id, wall: Date.now() };
        if (p.processor.startsWith("enhance")) {
          const prev = this.artifactsFor(p.meeting_id, "enhanced_notes")[0];
          const userText = (((p.params as Record<string, JsonValue>)?.user_notes as string) ?? "").split("\n").filter((l) => l.trim());
          const segs = (transcript.body as unknown as Transcript).segments;
          const blocks = [
            ...(userText.length ? userText : ["(no notes)"]).flatMap((line, i) => {
              const seg = segs[Math.min(i * 2 + 1, segs.length - 1)];
              return [
                { origin: "user", style: "bullet", level: 0, text: line, cites: [] },
                { origin: "ai", style: "bullet", level: 1, text: seg.text, cites: [{ artifact: transcript.id, segment: seg.idx, chars: null, time_ms: [seg.t0_ms, seg.t1_ms] }] },
              ];
            }),
            { origin: "ai", style: "heading", level: 0, text: "Tóm tắt thêm", cites: [] },
            ...segs.slice(-2).map((seg) => ({ origin: "ai", style: "bullet", level: 0, text: seg.text, cites: [{ artifact: transcript.id, segment: seg.idx, chars: null, time_ms: [seg.t0_ms, seg.t1_ms] }] })),
          ];
          this.addArtifact(fx.makeArtifact(ctx, "enhanced_notes", { lang: "vi", blocks }, { processor: p.processor, model: "ollama:qwen3.5:9b", inputs: [transcript.id] }, prev?.id ?? null));
        } else if (p.processor.startsWith("summarize")) {
          const prev = this.artifactsFor(p.meeting_id, "summary")[0];
          const prevBody = prev?.body as unknown as { sections: SummarySection[] } | undefined;
          const sections = (prevBody?.sections ?? []).map(
            (s) => [s.heading, s.bullets.map((b) => [b.text, b.cites.map((c) => c.segment)] as [string, number[]])] as [string, [string, number[]][]],
          );
          const bundle = fx.summaryBundle(ctx, transcript, sections, { model: String((p.params as Record<string, JsonValue>)?.model ?? "qwen3.5:9b"), supersedes: prev?.id ?? null });
          bundle.forEach((a) => this.addArtifact(a));
        }
      });
      return job;
    },
    "notes.get": function ({ doc }) {
      return { doc, updates_b64: [...(this.notes.get(doc) ?? [])] };
    },
    "notes.applyUpdate": function ({ doc, update_b64 }) {
      const list = this.notes.get(doc) ?? [];
      list.push(update_b64);
      this.notes.set(doc, list);
      return null;
    },
    "search.query": function ({ text, limit }) {
      const hits: SearchHit[] = [];
      for (const m of this.meetings) {
        const s = snippet(m.title, text);
        if (s) hits.push({ meeting_id: m.id, artifact_id: null, kind: "title", snippet: s, segment: null, score: 2 });
      }
      for (const a of this.artifacts) {
        if (a.kind === "transcript") {
          (a.body as unknown as Transcript).segments.forEach((seg) => {
            const s = snippet(seg.text, text);
            if (s) hits.push({ meeting_id: a.meeting_id, artifact_id: a.id, kind: "transcript", snippet: s, segment: seg.idx, score: 1 });
          });
        } else if (a.kind === "summary") {
          (a.body as unknown as { sections: SummarySection[] }).sections.forEach((sec) =>
            sec.bullets.forEach((b, i) => {
              const s = snippet(b.text, text);
              if (s) hits.push({ meeting_id: a.meeting_id, artifact_id: a.id, kind: "summary", snippet: s, segment: i, score: 1.5 });
            }),
          );
        }
      }
      return hits.sort((a, b) => b.score - a.score).slice(0, limit ?? 20);
    },
    "calendar.sources": function () {
      return { sources: [...this.sources] };
    },
    "calendar.addGoogle": async function () {
      await this.sleep(900);
      const n = this.sources.filter((s) => s.provider === "google").length;
      const email = n === 0 ? "nam.nguyen@gmail.com" : `nam.work${n}@almond.dev`;
      const src = { id: `google:${email}`, provider: "google", label: email, enabled: true };
      this.sources.push(src);
      return src;
    },
    "calendar.addIcs": function ({ url, label }) {
      const src = { id: `ics:${fold(url).replace(/[^a-z0-9]/g, "").slice(-8)}`, provider: "ics", label: label ?? url, enabled: true };
      this.sources.push(src);
      return src;
    },
    "calendar.importPhone": function () {
      if (this.plat !== "android") throw new ApiError(-32005, "calendar.importPhone is only available on Android");
      const src = { id: `phone:${fx.galaxy.id}`, provider: "phone", label: "Galaxy A26 · Samsung Calendar", enabled: true };
      this.sources.push(src);
      return src;
    },
    "calendar.remove": function ({ source }) {
      this.sources = this.sources.filter((s) => s.id !== source);
      return null;
    },
    "calendar.upcoming": function ({ hours }) {
      const now = Date.now();
      const until = now + (hours ?? 24) * fx.HOUR;
      return this.events
        .filter((e) => e.end > now && e.start < until)
        .sort((a, b) => a.start - b.start)
        .map((event) => ({ event, meeting_id: this.meetings.find((m) => m.calendar_event?.event_id === event.event_id)?.id ?? null }));
    },
    "calendar.refresh": function () {
      return null;
    },
    "mail.context": function () {
      return [
        {
          account: "nam.nguyen@gmail.com",
          thread_id: "t-1",
          subject: "Re: Lịch phát hành bản 0.2",
          participants: ["Trần Lan", "David Kim"],
          snippet: "Anh ơi, em nghĩ mình nên dời sang tuần sau để kịp test trên Android…",
          last_message_at: Date.now() - 20 * fx.HOUR,
          url: null,
        },
      ];
    },
    "actions.list": function () {
      return [
        { id: "gmail.draft_follow_up", title: "Draft follow-up email (Gmail)", requires: ["google:gmail.compose"] },
        { id: "calendar.create_follow_up", title: "Create follow-up event", requires: ["google:calendar.events"] },
        { id: "export.markdown", title: "Export Markdown", requires: [] },
      ];
    },
    "actions.propose": function ({ meeting_id }) {
      const a = this.artifactsFor(meeting_id, "action_proposals")[0];
      return (a?.body as unknown as Result<"actions.propose">) ?? { proposals: [] };
    },
    "actions.execute": async function ({ proposal }) {
      await this.sleep(700);
      const res = {
        proposal_id: proposal.id,
        ok: true,
        message: proposal.action === "gmail.draft_follow_up" ? "Draft saved in Gmail (not sent)." : proposal.action === "export.markdown" ? "Saved to Documents/Almond/notes.md" : "Event created.",
        url: proposal.action === "gmail.draft_follow_up" ? "https://mail.google.com/mail/#drafts" : null,
      };
      return res;
    },
    "pipeline.processors": function () {
      return [
        { name: "transcribe.whisper", version: "0.1.0", consumes: [], produces: ["transcript"], needs_audio: true, requires: ["stt-large"], description: "Whisper / PhoWhisper batch transcription" },
        { name: "summarize.llm", version: "0.1.0", consumes: ["transcript"], produces: ["summary"], needs_audio: false, requires: ["llm"], description: "Cited summary via Ollama" },
        { name: "enhance_notes.llm", version: "0.1.0", consumes: ["transcript"], produces: ["enhanced_notes"], needs_audio: false, requires: ["llm"], description: "Merge your notes with the transcript" },
      ];
    },
    "pipeline.registerExternal": function () {
      return [];
    },
    "jobs.list": function ({ status }) {
      return [...this.jobs.values()].filter((j) => !status || j.status === status);
    },
    "jobs.cancel": function ({ id }) {
      const j = this.jobs.get(id);
      if (j) {
        const c = { ...j, status: "cancelled" as const };
        this.jobs.set(id, c);
        this.emit("job.progress", c);
      }
      return null;
    },
    "verify.run": function ({ artifact_id }) {
      const a = this.artifacts.find((x) => x.id === artifact_id);
      return this.newJob(a?.meeting_id ?? "", "verify.run", [artifact_id]);
    },
    "verify.verifiers": function () {
      return [
        { name: "verify.citation_exists", version: "0.1.0", family: "symbolic", logical_forms: [], description: "Cited segment exists" },
        { name: "verify.numbers", version: "0.1.0", family: "symbolic", logical_forms: [], description: "Numbers/dates in claim appear in evidence" },
        { name: "verify.nli", version: "0.1.0", family: "neural", logical_forms: [], description: "LLM/NLI grounding judge" },
      ];
    },
    "sync.status": function () {
      return {
        this_device: fx.thisDevice,
        peers: this.peers.map((p) => ({ ...p })),
        syncing: this.syncing,
        auto_sync_while_open: Boolean(this.settings.get("sync.auto_while_open")),
        last_reports: this.lastReports,
      };
    },
    "sync.now": async function ({ peers }) {
      if (this.syncing) throw new ApiError(-32000, "sync already running");
      this.syncing = true;
      try {
        const targets = this.peers.filter((p) => p.paired && (!peers || peers.includes(p.device.id)));
        const reports = await Promise.all(
          targets.map(async (p, i) => {
            const t0 = Date.now();
            const peer = p.device.id;
            this.emit("sync.progress", { peer, phase: "connecting", done: 0, total: 0 });
            await this.sleep(500 + i * 300);
            const total = i === 0 ? 42 : 17;
            for (let d = 0; d <= total; d += Math.ceil(total / 6)) {
              this.emit("sync.progress", { peer, phase: "events", done: d, total });
              await this.sleep(250);
            }
            this.emit("sync.progress", { peer, phase: "events", done: total, total });
            const blobs = i === 0 ? 0 : 2;
            for (let d = 1; d <= blobs; d++) {
              this.emit("sync.progress", { peer, phase: "blobs", done: d, total: blobs });
              await this.sleep(500);
            }
            if (peer === fx.galaxy.id && !this.phoneDelivered) {
              this.phoneDelivered = true;
              const s = fx.phoneMeeting(Date.now());
              this.meetings.push(...s.meetings);
              s.artifacts.forEach((a) => this.addArtifact(a));
            }
            this.emit("sync.progress", { peer, phase: "done", done: total, total });
            p.last_synced = Date.now();
            p.last_seen = Date.now();
            return { peer, events_sent: i === 0 ? 12 : 5, events_received: total - (i === 0 ? 12 : 5), blobs_sent: 0, blobs_received: blobs, duration_ms: Date.now() - t0, error: null, status: "ok" as const };
          }),
        );
        this.lastReports = reports;
        return reports;
      } finally {
        this.syncing = false;
      }
    },
    "sync.pairBegin": function () {
      const words = ["almond", "river", "lotus", "basket", "cloud", "pepper"];
      const expires_at = Date.now() + 5 * fx.MIN;
      const ticket = { device: fx.thisDevice, addrs: ["192.168.1.10:47321"], secret: "K7Q2M4XZ", expires_at };
      return { qr_payload: `almond-pair:v1:${btoa(JSON.stringify(ticket))}`, words, expires_at };
    },
    "sync.pairAccept": async function ({ qr_payload, words }) {
      await this.sleep(800);
      if (!qr_payload && (!words || words.length !== 6 || words.some((w) => !/^[a-z]+$/.test(w)))) {
        throw new ApiError(-32602, "invalid pairing code: expected 6 words");
      }
      const peer: PeerInfo = {
        device: { id: `dev-${ulid().slice(-8).toLowerCase()}`, name: "Office desktop", platform: "linux", app_version: "0.1.0", capabilities: ["llm"], last_active: null },
        addrs: ["192.168.1.57:47321"],
        paired: true,
        last_seen: Date.now(),
        last_synced: null,
      };
      this.peers.push(peer);
      return peer;
    },
    "sync.unpair": function ({ device }) {
      this.peers = this.peers.filter((p) => p.device.id !== device);
      return null;
    },
    "sync.addAddr": function ({ device, addr }) {
      if (!device || !/^[^\s:]+:\d+$/.test(addr)) throw new ApiError(-32602, "need a device id and an ip:port address");
      return null;
    },
    "llm.status": function () {
      const remote = this.settings.get("llm.remote_ollama_url");
      const configured = typeof remote === "string" && remote.trim() ? remote.trim() : null;
      return {
        url: configured ?? "http://127.0.0.1:11434",
        remote: configured !== null,
        remote_configured: configured,
        models: configured ? ["gemma4:12b", "qwen3.5:4b"] : ["qwen3.5:4b"],
        error: null,
        offload: this.settings.get("processing.offload") === "prefer_peer" ? "prefer_peer" : "local",
      };
    },
    "storage.usage": function () {
      return { db_bytes: 3_400_000, blobs_bytes: 182_000_000, models_bytes: 2_900_000_000, audio_bytes: 176_000_000 };
    },
    "models.list": function () {
      return this.models.map((m) => ({ ...m }));
    },
    "models.download": function ({ id }) {
      const job = this.newJob("", "models.download", [], { id });
      void this.runJob(job, id, 4000, () => {
        const m = this.models.find((x) => x.id === id);
        if (m) m.installed = true;
      });
      return job;
    },
    "models.select": function (slot) {
      const slots = ((this.settings.get("models.slots") as unknown as ModelSlot[]) ?? []).filter((s) => !(s.task === slot.task && s.lang === slot.lang));
      slots.push(slot);
      this.settings.set("models.slots", slots as unknown as JsonValue);
      return null;
    },
    "models.slots": function () {
      return ((this.settings.get("models.slots") as unknown as ModelSlot[]) ?? []).map((s) => ({ ...s }));
    },
    "settings.get": function ({ key }) {
      return this.settings.get(key) ?? null;
    },
    "settings.set": function ({ key, value }) {
      this.settings.set(key, value);
      return null;
    },

    // ---- recipes (cookbook) ----
    "recipes.list": function () {
      return this.recipes.map((r) => ({ ...r }));
    },
    "recipes.save": function (p) {
      const cur = p.id ? this.recipes.find((r) => r.id === p.id) : undefined;
      if (p.id && !cur) throw new ApiError(-32004, `recipe not found: ${p.id}`);
      const next: RecipeInfo = {
        ...(cur ?? { id: "", name: "", description: "", icon: null, prompt: "", auto_run: false, builtin: false, updated_at: 0, hidden: false }),
        ...(p.name != null && { name: p.name.trim() }),
        ...(p.description != null && { description: p.description.trim() }),
        ...(p.icon != null && { icon: p.icon.trim() || null }),
        ...(p.prompt != null && { prompt: p.prompt.trim() }),
        ...(p.auto_run != null && { auto_run: p.auto_run }),
        ...(p.hidden != null && { hidden: p.hidden }),
        updated_at: Date.now(),
      };
      if (!next.name) throw new ApiError(-32602, "a recipe needs a name");
      if (!next.prompt) throw new ApiError(-32602, "a recipe needs a prompt");
      const changed = cur && (cur.name !== next.name || cur.description !== next.description || cur.icon !== next.icon || cur.prompt !== next.prompt);
      if (!cur || (cur.builtin && changed)) {
        // New recipe, or an edited built-in saved as a copy.
        const copy = { ...next, id: ulid(), builtin: false, hidden: false };
        this.recipes.push(copy);
        return { ...copy };
      }
      Object.assign(cur, next);
      return { ...cur };
    },
    "recipes.delete": function ({ id }) {
      if (id.startsWith("builtin:")) throw new ApiError(-32602, "built-in recipes can't be deleted; hide them instead");
      const i = this.recipes.findIndex((r) => r.id === id);
      if (i < 0) throw new ApiError(-32004, `recipe not found: ${id}`);
      this.recipes.splice(i, 1);
      return null;
    },
    "recipes.run": function ({ recipe_id, meeting_id, extra }) {
      const recipe = this.recipes.find((r) => r.id === recipe_id);
      if (!recipe) throw new ApiError(-32004, `recipe not found: ${recipe_id}`);
      this.meeting(meeting_id);
      const transcript = this.artifactsFor(meeting_id, "transcript")[0];
      if (!transcript) throw new ApiError(-32602, "this meeting has no transcript yet; run recipes once it is transcribed");
      const job = this.newJob(meeting_id, "recipe.llm", [transcript.id], { recipe_id, extra: extra ?? null }, ["llm"]);
      void this.runJob(job, recipe.name, 1500, () => {
        const prev = this.artifactsFor(meeting_id, "recipe_output").find((a) => (a.body as { recipe_id?: string }).recipe_id === recipe_id);
        const segs = (transcript.body as unknown as TranscriptBody).segments;
        this.addArtifact(
          fx.makeArtifact(
            { meeting_id, device: fx.thisDevice.id, wall: Date.now() },
            "recipe_output",
            mockRecipeOutput(recipe, transcript.id, segs, extra),
            { processor: `recipe.llm/${recipe_id}`, model: "ollama:gemma4:12b@mock", inputs: [transcript.id] },
            prev?.id ?? null,
          ),
        );
      });
      return job;
    },
    "recipes.outputs": function ({ meeting_id }) {
      const superseded = new Set(this.artifacts.map((a) => a.supersedes).filter(Boolean));
      const seen = new Set<string>();
      return this.artifactsFor(meeting_id, "recipe_output").filter((a) => {
        const rid = (a.body as { recipe_id?: string }).recipe_id ?? "";
        if (superseded.has(a.id) || seen.has(rid)) return false;
        seen.add(rid);
        return true;
      });
    },
  };
}
