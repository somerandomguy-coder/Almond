// The one typed surface the UI talks to. Mirrors docs/API.md method-for-method.
// Two implementations: TauriApi (real backend) and MockApi (in-memory, for dev/tests).
import type { ActionDescriptor } from "../gen/ActionDescriptor";
import type { ActionProposal } from "../gen/ActionProposal";
import type { ActionProposals } from "../gen/ActionProposals";
import type { ActionResult } from "../gen/ActionResult";
import type { Artifact } from "../gen/Artifact";
import type { AudioDeviceInfo } from "../gen/AudioDeviceInfo";
import type { CalendarSourceInfo } from "../gen/CalendarSourceInfo";
import type { CalendarSourcesResult } from "../gen/CalendarSourcesResult";
import type { CreateMeetingParams } from "../gen/CreateMeetingParams";
import type { DeviceId } from "../gen/DeviceId";
import type { EventRef } from "../gen/EventRef";
import type { Id } from "../gen/Id";
import type { AudioChannel } from "../gen/AudioChannel";
import type { Job } from "../gen/Job";
import type { JobProgress } from "../gen/JobProgress";
import type { JobStatus } from "../gen/JobStatus";
import type { MailThreadSummary } from "../gen/MailThreadSummary";
import type { Meeting } from "../gen/Meeting";
import type { MeetingDetail } from "../gen/MeetingDetail";
import type { MeetingQuery } from "../gen/MeetingQuery";
import type { MeetingReminder } from "../gen/MeetingReminder";
import type { ModelEntry } from "../gen/ModelEntry";
import type { ModelSlot } from "../gen/ModelSlot";
import type { NotesSnapshot } from "../gen/NotesSnapshot";
import type { PairBeginResult } from "../gen/PairBeginResult";
import type { PeerInfo } from "../gen/PeerInfo";
import type { ProcessorDescriptor } from "../gen/ProcessorDescriptor";
import type { RecordingStatus } from "../gen/RecordingStatus";
import type { RerunParams } from "../gen/RerunParams";
import type { SearchHit } from "../gen/SearchHit";
import type { StartRecordingParams } from "../gen/StartRecordingParams";
import type { SyncProgress } from "../gen/SyncProgress";
import type { SyncReport } from "../gen/SyncReport";
import type { SyncStatus } from "../gen/SyncStatus";
import type { TranscriptUpdate } from "../gen/TranscriptUpdate";
import type { UpcomingEvent } from "../gen/UpcomingEvent";
import type { UpdateMeetingParams } from "../gen/UpdateMeetingParams";
import type { VerifierDescriptor } from "../gen/VerifierDescriptor";
import type { JsonValue } from "../gen/serde_json/JsonValue";

/** Method name → [params, result]. Keys are exactly the docs/API.md method names. */
export interface ApiMethods {
  "meetings.list": [MeetingQuery, Meeting[]];
  "meetings.get": [{ id: Id }, MeetingDetail];
  "meetings.create": [CreateMeetingParams, Meeting];
  "meetings.update": [UpdateMeetingParams, Meeting];
  "meetings.delete": [{ id: Id }, null];
  /** Import an audio/video file on this device as a new meeting (resolves when it is stored; processing follows). */
  "meetings.import": [ImportParams, Meeting];
  /** Stop a running import (id from `import.progress`); true if one was running. */
  "meetings.importCancel": [{ meeting_id: Id }, boolean];
  "recording.start": [StartRecordingParams, RecordingStatus];
  "recording.stop": [Record<string, never>, RecordingStatus];
  "recording.status": [Record<string, never>, RecordingStatus];
  "audio.devices": [Record<string, never>, AudioDeviceInfo[]];
  /** Playable URL (Tauri asset protocol) of a meeting's audio; mic + system mixed when no channel is given; null if not on this device. */
  "audio.url": [{ meeting_id: Id; channel?: AudioChannel | null }, { url: string | null }];
  "artifacts.list": [{ meeting_id: Id; kind?: string }, Artifact[]];
  "artifacts.get": [{ id: Id }, Artifact];
  "artifacts.rerun": [RerunParams, Job];
  "notes.get": [{ doc: Id }, NotesSnapshot];
  "notes.applyUpdate": [{ doc: Id; update_b64: string }, null];
  "search.query": [{ text: string; limit?: number }, SearchHit[]];
  "calendar.sources": [Record<string, never>, CalendarSourcesResult];
  "calendar.addGoogle": [{ gmail?: boolean; login_hint?: string }, CalendarSourceInfo];
  "calendar.addIcs": [{ url: string; label?: string }, CalendarSourceInfo];
  "calendar.importPhone": [Record<string, never>, CalendarSourceInfo];
  "calendar.remove": [{ source: string }, null];
  "calendar.upcoming": [{ hours?: number }, UpcomingEvent[]];
  "calendar.refresh": [Record<string, never>, null];
  "mail.context": [{ event: EventRef }, MailThreadSummary[]];
  "actions.list": [Record<string, never>, ActionDescriptor[]];
  "actions.propose": [{ meeting_id: Id }, ActionProposals];
  "actions.execute": [{ proposal: ActionProposal }, ActionResult];
  "pipeline.processors": [Record<string, never>, ProcessorDescriptor[]];
  "pipeline.registerExternal": [{ command?: string; url?: string }, ProcessorDescriptor[]];
  "jobs.list": [{ status?: JobStatus }, JobProgress[]];
  "jobs.cancel": [{ id: Id }, null];
  "verify.run": [{ artifact_id: Id; verifiers?: string[] }, Job];
  "verify.verifiers": [Record<string, never>, VerifierDescriptor[]];
  "sync.status": [Record<string, never>, SyncStatus];
  "sync.now": [{ peers?: DeviceId[] }, SyncReport[]];
  "sync.pairBegin": [Record<string, never>, PairBeginResult];
  "sync.pairAccept": [{ qr_payload?: string; words?: string[]; device?: DeviceId }, PeerInfo];
  "sync.unpair": [{ device: DeviceId }, null];
  "sync.addAddr": [{ device: DeviceId; addr: string }, null];
  "models.list": [Record<string, never>, ModelEntry[]];
  "models.download": [{ id: string }, Job];
  "models.select": [ModelSlot, null];
  "models.slots": [Record<string, never>, ModelSlot[]];
  "storage.usage": [Record<string, never>, StorageUsage];
  "settings.get": [{ key: string }, JsonValue];
  "settings.set": [{ key: string; value: JsonValue }, null];
}

/** `meetings.import` params (Rust: `almond_core::capture::import::ImportParams`). */
export interface ImportParams {
  /** Absolute path of the file on this device. */
  path: string;
  /** Default: the file name without extension. */
  title?: string | null;
  /** Spoken language; default: detected by transcription. */
  language?: "vi" | "en" | null;
}

export type ImportStage = "decoding" | "storing" | "done" | "failed" | "cancelled";

/** `import.progress` payload (Rust: `almond_core::capture::import::ImportProgress`). */
export interface ImportProgress {
  /** Id the imported meeting gets (for `meetings.importCancel`). */
  meeting_id: Id;
  path: string;
  file_name: string;
  stage: ImportStage;
  /** 0..1 when known. */
  progress: number | null;
  processed_ms: number;
  duration_ms: number | null;
  decoder: string | null;
  error: string | null;
}

/** `storage.usage`: bytes on this device. `audio_bytes` is the audio part of `blobs_bytes`. */
export interface StorageUsage {
  db_bytes: number;
  blobs_bytes: number;
  models_bytes: number;
  audio_bytes: number;
}

export type Method = keyof ApiMethods;
export type Params<M extends Method> = ApiMethods[M][0];
export type Result<M extends Method> = ApiMethods[M][1];

/** Notification name → payload (server → client). */
export interface ApiNotifications {
  "recording.status": RecordingStatus;
  "transcript.update": TranscriptUpdate;
  "job.progress": JobProgress;
  "artifact.added": Artifact;
  "sync.progress": SyncProgress;
  "calendar.reminder": MeetingReminder;
  "import.progress": ImportProgress;
}
export type NotificationName = keyof ApiNotifications;

export type Unlisten = () => void;

export interface AlmondApi {
  /** "tauri" or "mock" — shown in the UI so it's obvious which one is live. */
  readonly kind: "tauri" | "mock";
  /** The OS/platform the backend runs on ("windows" | "linux" | "android" | "web"). */
  platform(): Promise<string>;
  call<M extends Method>(method: M, params: Params<M>): Promise<Result<M>>;
  on<N extends NotificationName>(name: N, cb: (payload: ApiNotifications[N]) => void): Unlisten;
}

/** Well-known settings keys used by the UI (settings are local-only, never synced). */
export const SettingKeys = {
  language: "ui.language",
  theme: "ui.theme",
  reminderLeadMin: "calendar.reminder_lead_min",
  autoSyncWhileOpen: "sync.auto_while_open",
  googleOAuthClient: "google.oauth_client",
} as const;

/** Well-known artifact kinds (mirror of `ArtifactKind` consts in almond-types). */
export const Kinds = {
  transcript: "transcript",
  summary: "summary",
  enhancedNotes: "enhanced_notes",
  actionItems: "action_items",
  title: "title",
  claims: "claims",
  verdicts: "verdicts",
  actionProposals: "action_proposals",
  actionResult: "action_result",
} as const;

/** Processor names the UI triggers explicitly. */
export const Processors = {
  enhanceNotes: "enhance_notes.llm",
  summarize: "summarize.llm",
} as const;
