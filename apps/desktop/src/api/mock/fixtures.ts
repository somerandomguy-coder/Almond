// Realistic bilingual sample data, mirroring crates/almond-types/src/mock.rs `fixtures`
// (same Q4 planning meeting, same deliberately-wrong "300 triệu" summary bullet).
import type { ActionItems } from "../../gen/ActionItems";
import type { ActionProposals } from "../../gen/ActionProposals";
import type { Artifact } from "../../gen/Artifact";
import type { CalendarEvent } from "../../gen/CalendarEvent";
import type { CalendarSourceInfo } from "../../gen/CalendarSourceInfo";
import type { Claims } from "../../gen/Claims";
import type { DeviceInfo } from "../../gen/DeviceInfo";
import type { EnhancedNotes } from "../../gen/EnhancedNotes";
import type { Meeting } from "../../gen/Meeting";
import type { ModelEntry } from "../../gen/ModelEntry";
import type { ModelSlot } from "../../gen/ModelSlot";
import type { PeerInfo } from "../../gen/PeerInfo";
import type { Provenance } from "../../gen/Provenance";
import type { Segment } from "../../gen/Segment";
import type { SpanRef } from "../../gen/SpanRef";
import type { Summary } from "../../gen/Summary";
import type { Transcript } from "../../gen/Transcript";
import type { VerdictLabel } from "../../gen/VerdictLabel";
import type { Verdicts } from "../../gen/Verdicts";
import type { JsonValue } from "../../gen/serde_json/JsonValue";
import { ulid } from "../../lib/ids";

export const MIN = 60_000;
export const HOUR = 60 * MIN;
export const DAY = 24 * HOUR;

export const thisDevice: DeviceInfo = {
  id: "dev-nam-pc-7f3a9c",
  name: "Nam's PC",
  platform: "windows",
  app_version: "0.1.0",
  capabilities: ["gpu", "llm", "llm:qwen3.5:9b", "stt:vi", "stt:en", "stt-large", "diarize"],
};

export const fedora: DeviceInfo = {
  id: "dev-fedora-laptop-2b81e0",
  name: "Fedora laptop",
  platform: "linux",
  app_version: "0.1.0",
  capabilities: ["llm", "llm:gemma4:e4b", "stt:vi"],
};

export const galaxy: DeviceInfo = {
  id: "dev-galaxy-a26-c45d12",
  name: "Galaxy A26",
  platform: "android",
  app_version: "0.1.0",
  capabilities: ["stt:vi"],
};

export function peers(now: number): PeerInfo[] {
  return [
    { device: fedora, addrs: ["192.168.1.23:47321"], paired: true, last_seen: now - 2 * MIN, last_synced: now - 2 * DAY },
    { device: galaxy, addrs: ["192.168.1.41:47321"], paired: true, last_seen: now - 30_000, last_synced: now - 5 * DAY },
  ];
}

export function calendarSources(): CalendarSourceInfo[] {
  return [
    { id: "google:nam.nguyen@gmail.com", provider: "google", label: "nam.nguyen@gmail.com", enabled: true },
    { id: "ics:3f9a1c", provider: "ics", label: "Lịch nghỉ lễ Việt Nam", enabled: true },
  ];
}

export function models(): ModelEntry[] {
  return [
    { id: "whisper:phowhisper-large", kind: "stt", installed: true, size_bytes: 3_090_000_000, languages: ["vi"], note: "Best Vietnamese accuracy (GPU)" },
    { id: "sherpa:zipformer-vi-int8", kind: "stt", installed: true, size_bytes: 72_000_000, languages: ["vi"], note: "Streaming, live captions" },
    { id: "whisper:large-v3-turbo", kind: "stt", installed: false, size_bytes: 1_620_000_000, languages: ["en", "vi", "zh"], note: "Multilingual" },
    { id: "ollama:qwen3.5:9b", kind: "llm", installed: true, size_bytes: 6_600_000_000, languages: [], note: "Default summarizer on PC" },
    { id: "ollama:gemma4:e4b", kind: "llm", installed: true, size_bytes: 3_300_000_000, languages: [], note: "Light, for laptops" },
    { id: "ollama:nomic-embed-text", kind: "embed", installed: true, size_bytes: 274_000_000, languages: [], note: null },
    { id: "pyannote:segmentation-3.0", kind: "diarize", installed: false, size_bytes: 6_000_000, languages: [], note: null },
  ];
}

export function modelSlots(): ModelSlot[] {
  return [
    { task: "stt", lang: "vi", model: "whisper:phowhisper-large" },
    { task: "stt", lang: "en", model: "whisper:large-v3-turbo" },
    { task: "stt_live", lang: "vi", model: "sherpa:zipformer-vi-int8" },
    { task: "llm_summary", lang: null, model: "ollama:qwen3.5:9b" },
    { task: "llm_verify", lang: null, model: "ollama:qwen3.5:9b" },
    { task: "embed", lang: null, model: "ollama:nomic-embed-text" },
  ];
}

function startOfDay(t: number) {
  const d = new Date(t);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

export function calendarEvents(now: number): CalendarEvent[] {
  const base = {
    source: "google:nam.nguyen@gmail.com",
    calendar_id: "primary",
    calendar_name: "Nam",
    location: null,
    all_day: false,
    status: "confirmed",
    updated: now - DAY,
    organizer: "nam.nguyen@gmail.com",
  };
  const me = { email: "nam.nguyen@gmail.com", name: "Nam Nguyễn", response: "accepted", is_self: true };
  return [
    {
      ...base,
      event_id: "evt-standup",
      title: "Standup nhóm Almond",
      description: "Cập nhật tiến độ sync + giao diện.",
      start: now + 4 * MIN,
      end: now + 19 * MIN,
      attendees: [me, { email: "lan.tran@almond.dev", name: "Trần Lan", response: "accepted", is_self: false }, { email: "david@almond.dev", name: "David Kim", response: "tentative", is_self: false }],
      conference_url: "https://meet.google.com/abc-defg-hij",
      conference_app: "google_meet",
    },
    {
      ...base,
      event_id: "evt-1on1",
      title: "1:1 with Minh",
      description: null,
      start: now + 2 * HOUR,
      end: now + 2 * HOUR + 30 * MIN,
      attendees: [me, { email: "minh.le@almond.dev", name: "Lê Minh", response: "accepted", is_self: false }],
      conference_url: "https://zoom.us/j/123456789",
      conference_app: "zoom",
    },
    {
      ...base,
      event_id: "evt-review",
      title: "Product review — Q4 roadmap",
      description: "Agenda: mobile launch, Tết campaign, hiring.",
      start: startOfDay(now) + DAY + 10 * HOUR,
      end: startOfDay(now) + DAY + 11 * HOUR,
      location: "Phòng họp Sen, tầng 3",
      attendees: [me, { email: "lan.tran@almond.dev", name: "Trần Lan", response: "needs_action", is_self: false }],
      conference_url: null,
      conference_app: null,
    },
  ];
}

// ---------------------------------------------------------------------------------------------

type Line = [speaker: string, lang: string, text: string];

export const Q4_LINES: Line[] = [
  ["me", "vi", "Chào mọi người, hôm nay mình bàn về ngân sách Q4 và kế hoạch tuyển dụng."],
  ["S1", "vi", "Ngân sách marketing là 200 triệu đồng, tăng 15% so với Q3."],
  ["S2", "en", "We should hire two backend engineers by December."],
  ["me", "vi", "Đồng ý. David, anh lo phần job description được không?"],
  ["S2", "en", "Sure, I'll draft it this week and share it in the channel."],
  ["S1", "vi", "Chiến dịch Tết sẽ bắt đầu từ ngày 15 tháng 12."],
  ["me", "en", "Let's keep the launch date for the mobile app on January 10th."],
  ["S1", "vi", "Em lo là đội QA hơi thiếu người, cần thêm một bạn tester."],
  ["me", "vi", "Ok, Lan sẽ gửi bản kế hoạch chi tiết trước thứ Sáu."],
  ["S2", "en", "And we'll review the budget again in the first week of January."],
];

const DESIGN_LINES: Line[] = [
  ["S1", "en", "The new onboarding flow cuts the number of steps from seven to four."],
  ["me", "en", "Nice. Did we test it with Vietnamese copy? Diacritics broke the layout last time."],
  ["S1", "en", "Yes, we switched to Inter and the line height is fine now."],
  ["S2", "vi", "Màn hình chọn ngôn ngữ nên để tiếng Việt mặc định."],
  ["me", "en", "Agreed. Let's ship it behind a flag next Tuesday."],
  ["S2", "en", "I'll prepare the usability test notes by Monday."],
];

const INTERVIEW_LINES: Line[] = [
  ["me", "vi", "Bạn giới thiệu ngắn về kinh nghiệm làm Rust nhé."],
  ["S1", "vi", "Em có ba năm làm backend, hai năm gần đây chủ yếu viết Rust với tokio."],
  ["me", "vi", "Bạn đã làm việc với SQLite hay CRDT bao giờ chưa?"],
  ["S1", "vi", "Em dùng SQLite cho một app offline-first, còn CRDT thì mới đọc về Automerge."],
  ["me", "vi", "Ok, mức lương mong muốn của bạn là bao nhiêu?"],
  ["S1", "vi", "Khoảng 45 triệu một tháng ạ."],
];

/** Script streamed as live captions while "recording" in the mock. */
export const LIVE_LINES: Line[] = [
  ["me", "vi", "Ok mọi người, mình bắt đầu standup nhé."],
  ["S1", "vi", "Hôm qua em xong phần pairing bằng mã QR, hôm nay làm tiếp phần sync blob."],
  ["S2", "en", "I fixed the WASAPI loopback glitch, audio is clean on Windows now."],
  ["me", "vi", "Tốt quá. Còn vấn đề pin trên Galaxy A26 thì sao?"],
  ["S1", "vi", "Cần hướng dẫn người dùng tắt tối ưu hoá pin cho Almond."],
  ["S2", "en", "Should we move the release to next Wednesday, the 18th?"],
  ["me", "vi", "Ừ, dời sang thứ Tư ngày 18. Lan cập nhật lại lịch giúp anh."],
  ["S1", "vi", "Dạ, em sẽ gửi lịch mới trong hôm nay."],
  ["me", "en", "Great, thanks everyone. Let's wrap up."],
];

export const SPEAKERS = [
  { id: "me", name: "Nam Nguyễn", email: "nam.nguyen@gmail.com", is_me: true },
  { id: "S1", name: "Trần Lan", email: "lan.tran@almond.dev", is_me: false },
  { id: "S2", name: "David Kim", email: "david@almond.dev", is_me: false },
];

export function segmentsFrom(lines: Line[], stepMs = 6500): Segment[] {
  return lines.map(([spk, lang, text], i) => ({
    idx: i,
    t0_ms: i * stepMs,
    t1_ms: i * stepMs + stepMs - 600,
    channel: spk === "me" ? "mic" : "system",
    speaker: spk,
    lang,
    text,
    confidence: 0.86 + ((i * 7) % 12) / 100,
    words: null,
  }));
}

export interface ArtifactFactoryCtx {
  meeting_id: string;
  device: string;
  wall: number;
}

let logical = 0;
export function makeArtifact(
  ctx: ArtifactFactoryCtx,
  kind: string,
  body: unknown,
  provenance: Partial<Provenance> & { processor: string },
  supersedes: string | null = null,
): Artifact {
  return {
    id: ulid(ctx.wall),
    meeting_id: ctx.meeting_id,
    kind,
    schema: 1,
    body: body as JsonValue,
    provenance: {
      processor_version: "0.1.0",
      model: null,
      params_hash: null,
      inputs: [],
      ...provenance,
    },
    supersedes,
    created_at: { wall_ms: ctx.wall, logical: logical++ },
    device: ctx.device,
  };
}

const cite = (artifact: string, segment: number, segs: Segment[]): SpanRef => {
  const s = segs[Math.min(segment, segs.length - 1)];
  return { artifact, segment: s.idx, chars: null, time_ms: [s.t0_ms, s.t1_ms] };
};

type BulletSpec = [text: string, cites: number[], verdict?: [VerdictLabel, number, string]];
type SectionSpec = [heading: string, bullets: BulletSpec[]];

/** Build summary + claims + verdicts for a given transcript. */
export function summaryBundle(
  ctx: ArtifactFactoryCtx,
  transcript: Artifact,
  sections: SectionSpec[],
  opts: { model: string; supersedes?: string | null; lang?: string; device?: string },
): Artifact[] {
  const segs = (transcript.body as unknown as Transcript).segments;
  const summaryBody: Summary = {
    lang: opts.lang ?? "vi",
    sections: sections.map(([heading, bullets]) => ({
      heading,
      bullets: bullets.map(([text, cs]) => ({ text, cites: cs.map((c) => cite(transcript.id, c, segs)) })),
    })),
  };
  const dctx = { ...ctx, device: opts.device ?? ctx.device };
  const summary = makeArtifact(
    dctx,
    "summary",
    summaryBody,
    { processor: "summarize.llm", model: `ollama:${opts.model}`, params_hash: "b3:summ-vi-v2", inputs: [transcript.id] },
    opts.supersedes ?? null,
  );
  const flat = sections.flatMap(([, b]) => b);
  const claimsBody: Claims = {
    claims: flat.map(([text, cs], i) => ({
      id: `c${i + 1}`,
      text,
      source: { artifact: summary.id, segment: i, chars: null, time_ms: null },
      evidence: cs.map((c) => cite(transcript.id, c, segs)),
      logical_form_kind: null,
      logical_form: null,
    })),
  };
  const claims = makeArtifact({ ...ctx, wall: ctx.wall + 1 }, "claims", claimsBody, {
    processor: "verify.extract_claims",
    model: "ollama:qwen3.5:9b",
    inputs: [summary.id],
  });
  const verdictsBody: Verdicts = {
    verdicts: flat.flatMap(([, cs, v], i) => {
      const [label, confidence, explanation] = v ?? (["supported", 0.93, "Cited span states the same fact."] as const);
      const ev = cs.map((c) => cite(transcript.id, c, segs));
      const out: Verdicts["verdicts"] = [
        { claims_artifact: claims.id, claim_id: `c${i + 1}`, verifier: "verify.nli", label, confidence, evidence: ev, explanation, trace: null },
      ];
      if (cs.length) {
        out.push({
          claims_artifact: claims.id,
          claim_id: `c${i + 1}`,
          verifier: "verify.citation_exists",
          label: "supported",
          confidence: 1,
          evidence: ev,
          explanation: "Cited segment exists in the transcript.",
          trace: null,
        });
      }
      if (label === "contradicted") {
        out.push({
          claims_artifact: claims.id,
          claim_id: `c${i + 1}`,
          verifier: "verify.numbers",
          label: "contradicted",
          confidence: 1,
          evidence: ev,
          explanation: explanation,
          trace: { rule: "number_mismatch", claim: 300, evidence: 200 } as unknown as JsonValue,
        });
      }
      return out;
    }),
  };
  const verdicts = makeArtifact({ ...ctx, wall: ctx.wall + 2 }, "verdicts", verdictsBody, {
    processor: "verify.run",
    inputs: [claims.id, transcript.id],
  });
  return [summary, claims, verdicts];
}

export function transcriptArtifact(ctx: ArtifactFactoryCtx, segments: Segment[], model = "whisper:phowhisper-large"): Artifact {
  const body: Transcript = { segments, speakers: SPEAKERS };
  return makeArtifact(ctx, "transcript", body, { processor: "transcribe.whisper", model });
}

export const Q4_SUMMARY_V1: SectionSpec[] = [
  [
    "Quyết định",
    [
      ["Ngân sách marketing Q4 là 300 triệu đồng.", [1], ["contradicted", 0.97, "Transcript says 200 triệu đồng, not 300."]],
      ["Tuyển 2 kỹ sư backend trước tháng 12.", [2]],
      ["Ngày ra mắt ứng dụng di động giữ nguyên 10/1.", [6]],
    ],
  ],
  [
    "Việc cần làm",
    [
      ["Lan gửi kế hoạch chi tiết trước thứ Sáu.", [8]],
      ["David soạn job description trong tuần này.", [3, 4]],
    ],
  ],
  [
    "Rủi ro",
    [
      ["Đội QA thiếu người, cần thêm một tester.", [7], ["needs_review", 0.55, "Speaker expresses a concern, not a decision."]],
      ["Chiến dịch Tết đã được duyệt ngân sách riêng.", [], ["unverifiable", 0.8, "No transcript segment mentions a separate Tết budget."]],
    ],
  ],
];

export const Q4_SUMMARY_V2: SectionSpec[] = [
  [
    "Quyết định",
    [
      ["Ngân sách marketing Q4 là 200 triệu đồng, tăng 15% so với Q3.", [1]],
      ["Tuyển 2 kỹ sư backend trước tháng 12.", [2]],
      ["Giữ ngày ra mắt ứng dụng di động vào 10/1.", [6]],
      ["Chiến dịch Tết bắt đầu từ 15/12.", [5]],
    ],
  ],
  [
    "Việc cần làm",
    [
      ["Lan gửi kế hoạch chi tiết trước thứ Sáu.", [8]],
      ["David soạn job description trong tuần này và chia sẻ trong channel.", [3, 4]],
      ["Xem lại ngân sách vào tuần đầu tháng 1.", [9]],
    ],
  ],
  ["Rủi ro", [["Lan lo đội QA thiếu người; đề xuất thêm một tester.", [7], ["needs_review", 0.62, "A concern was raised; no decision recorded."]]]],
];

export function q4Extras(ctx: ArtifactFactoryCtx, transcript: Artifact, now: number): Artifact[] {
  const segs = (transcript.body as unknown as Transcript).segments;
  const c = (i: number) => cite(transcript.id, i, segs);
  const enhanced: EnhancedNotes = {
    lang: "vi",
    blocks: [
      { origin: "user", style: "heading", level: 0, text: "Ngân sách Q4", cites: [] },
      { origin: "user", style: "bullet", level: 0, text: "ngân sách mkt", cites: [] },
      { origin: "ai", style: "bullet", level: 1, text: "Marketing: 200 triệu đồng, tăng 15% so với Q3", cites: [c(1)] },
      { origin: "ai", style: "bullet", level: 1, text: "Xem lại ngân sách vào tuần đầu tháng 1", cites: [c(9)] },
      { origin: "user", style: "heading", level: 0, text: "Tuyển dụng", cites: [] },
      { origin: "user", style: "bullet", level: 0, text: "tuyển 2 BE trước T12", cites: [] },
      { origin: "ai", style: "bullet", level: 1, text: "David soạn job description trong tuần này", cites: [c(4)] },
      { origin: "ai", style: "heading", level: 0, text: "Rủi ro", cites: [] },
      { origin: "user", style: "bullet", level: 0, text: "QA thiếu người?", cites: [] },
      { origin: "ai", style: "bullet", level: 1, text: "Lan đề xuất thêm một tester", cites: [c(7)] },
      { origin: "ai", style: "todo", level: 0, text: "Lan gửi kế hoạch chi tiết trước thứ Sáu", cites: [c(8)] },
    ],
  };
  const friday = new Date(now);
  friday.setDate(friday.getDate() + ((5 - friday.getDay() + 7) % 7 || 7));
  const items: ActionItems = {
    items: [
      { text: "Gửi bản kế hoạch chi tiết", owner: "Trần Lan", due: friday.toISOString().slice(0, 10), cites: [c(8)] },
      { text: "Draft backend job description", owner: "David Kim", due: "this week", cites: [c(4)] },
      { text: "Xem lại ngân sách Q1", owner: "Nam Nguyễn", due: "first week of January", cites: [c(9)] },
    ],
  };
  const proposals: ActionProposals = {
    proposals: [
      {
        id: "prop-gmail-1",
        action: "gmail.draft_follow_up",
        title: "Draft follow-up email to Lan, David",
        preview:
          "Chào Lan, David,\n\nCảm ơn mọi người đã tham gia buổi họp kế hoạch Q4. Tóm tắt:\n• Ngân sách marketing: 200 triệu đồng\n• Tuyển 2 kỹ sư backend trước tháng 12\n• Lan gửi kế hoạch chi tiết trước thứ Sáu\n\nNam",
        payload: { to: ["lan.tran@almond.dev", "david@almond.dev"], subject: "Follow-up: Họp kế hoạch Q4" },
      },
      {
        id: "prop-cal-1",
        action: "calendar.create_follow_up",
        title: "Create “Budget review” on the first Monday of January",
        preview: "Budget review — 60 min — Lan, David, Nam",
        payload: { title: "Budget review", duration_min: 60 },
      },
      { id: "prop-md-1", action: "export.markdown", title: "Export notes as Markdown", preview: "# Họp kế hoạch Q4\n\n## Quyết định\n- …", payload: {} },
    ],
  };
  return [
    makeArtifact({ ...ctx, wall: ctx.wall + 10 }, "enhanced_notes", enhanced, { processor: "enhance_notes.llm", model: "ollama:qwen3.5:9b", inputs: [transcript.id] }),
    makeArtifact({ ...ctx, wall: ctx.wall + 11 }, "action_items", items, { processor: "action_items.llm", model: "ollama:qwen3.5:9b", inputs: [transcript.id] }),
    makeArtifact({ ...ctx, wall: ctx.wall + 12 }, "action_proposals", proposals, { processor: "actions.propose", inputs: [transcript.id] }),
  ];
}

export interface Seed {
  meetings: Meeting[];
  artifacts: Artifact[];
}

function meeting(partial: Partial<Meeting> & Pick<Meeting, "title" | "started_at">): Meeting {
  return {
    id: ulid(partial.started_at),
    ended_at: partial.started_at + 30 * MIN,
    kind: { type: "online", app: "google_meet" },
    calendar_event: null,
    languages: ["vi", "en"],
    audio: [],
    notes_doc: ulid(partial.started_at + 1),
    tags: [],
    ...partial,
  };
}

export function seed(now: number): Seed {
  const day0 = startOfDay(now);
  const q4 = meeting({ title: "Họp kế hoạch Q4", started_at: day0 - DAY + 14 * HOUR, tags: ["kế hoạch", "Q4"] });
  const design = meeting({
    title: "Design sync — onboarding flow",
    started_at: day0 - 3 * DAY + 9.5 * HOUR,
    languages: ["en", "vi"],
    kind: { type: "online", app: "zoom" },
  });
  const interview = meeting({
    title: "Phỏng vấn ứng viên backend",
    started_at: day0 - 5 * DAY + 15 * HOUR,
    languages: ["vi"],
    kind: { type: "in_person" },
    tags: ["tuyển dụng"],
  });

  const artifacts: Artifact[] = [];
  // Q4: transcript on PC, v1 summary from the Fedora laptop (gemma, wrong number), v2 rerun on PC.
  const qctx = { meeting_id: q4.id, device: thisDevice.id, wall: q4.ended_at! + MIN };
  const qt = transcriptArtifact(qctx, segmentsFrom(Q4_LINES));
  const v1 = summaryBundle({ ...qctx, wall: qctx.wall + 100 }, qt, Q4_SUMMARY_V1, { model: "gemma4:e4b", device: fedora.id });
  const v2 = summaryBundle({ ...qctx, wall: qctx.wall + 5 * MIN }, qt, Q4_SUMMARY_V2, { model: "qwen3.5:9b", supersedes: v1[0].id });
  artifacts.push(qt, ...v1, ...v2, ...q4Extras({ ...qctx, wall: qctx.wall + 200 }, qt, now));

  const dctx = { meeting_id: design.id, device: thisDevice.id, wall: design.ended_at! + MIN };
  const dt = transcriptArtifact(dctx, segmentsFrom(DESIGN_LINES), "whisper:large-v3-turbo");
  artifacts.push(
    dt,
    ...summaryBundle(
      { ...dctx, wall: dctx.wall + 100 },
      dt,
      [
        ["Decisions", [["Onboarding drops from seven steps to four.", [0]], ["Ship behind a flag next Tuesday.", [4]], ["Vietnamese is the default language on the picker.", [3]]]],
        ["Follow-ups", [["Usability test notes by Monday.", [5]]]],
      ],
      { model: "qwen3.5:9b", lang: "en" },
    ),
  );

  const ictx = { meeting_id: interview.id, device: galaxy.id, wall: interview.ended_at! + MIN };
  const it = transcriptArtifact(ictx, segmentsFrom(INTERVIEW_LINES), "sherpa:zipformer-vi-int8");
  artifacts.push(
    it,
    ...summaryBundle(
      { ...ictx, wall: ictx.wall + 100 },
      it,
      [["Ứng viên", [["3 năm backend, 2 năm gần đây viết Rust với tokio.", [1]], ["Đã dùng SQLite cho app offline-first; chưa làm CRDT thực tế.", [3]], ["Mong muốn lương khoảng 45 triệu/tháng.", [5]]]]],
      { model: "qwen3.5:9b", device: thisDevice.id },
    ),
  );

  return { meetings: [q4, design, interview], artifacts };
}

/** A meeting that "arrives" from the Galaxy A26 during the first mock sync. */
export function phoneMeeting(now: number): Seed {
  const m = meeting({
    title: "Gặp khách hàng ở quán cà phê",
    started_at: now - 3 * HOUR,
    languages: ["vi"],
    kind: { type: "in_person" },
  });
  const ctx = { meeting_id: m.id, device: galaxy.id, wall: m.ended_at! + MIN };
  const t = transcriptArtifact(
    ctx,
    segmentsFrom([
      ["S1", "vi", "Bên anh cần xuất biên bản họp ra Word mỗi tuần."],
      ["me", "vi", "Dạ, bản tiếp theo bên em sẽ hỗ trợ xuất Markdown và Word."],
      ["S1", "vi", "Giá cho gói đội nhóm mười người là bao nhiêu em?"],
      ["me", "vi", "Bên em sẽ gửi báo giá trước thứ Tư tuần sau."],
    ]),
    "sherpa:zipformer-vi-int8",
  );
  return { meetings: [m], artifacts: [t] };
}
