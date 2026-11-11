// The one typed surface the UI talks to. Mirrors docs/API.md method-for-method.
// Two implementations: TauriApi (real backend) and MockApi (in-memory, for dev/tests).
import type { ActionDescriptor } from "../gen/ActionDescriptor";
import type { ActionProposal } from "../gen/ActionProposal";
import type { ActionProposals } from "../gen/ActionProposals";
import type { ActionResult } from "../gen/ActionResult";
import type { Artifact } from "../gen/Artifact";
import type { AudioDeviceInfo } from "../gen/AudioDeviceInfo";
import type { CalendarSourceInfo } from "../gen/CalendarSourceInfo";
import type { ChatEntry } from "../gen/ChatEntry";
import type { ChatScope } from "../gen/ChatScope";
import type { ChatThread } from "../gen/ChatThread";
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
import type { Recipe } from "../gen/Recipe";
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
// people
import type { LabelSource } from "../gen/LabelSource";
import type { Person } from "../gen/Person";
import type { SpeakerLabel } from "../gen/SpeakerLabel";
import type { SpeakerLabels } from "../gen/SpeakerLabels";
import type { VoicePrint } from "../gen/VoicePrint";

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
  "llm.status": [{ refresh?: boolean }, LlmStatus];
  "storage.usage": [Record<string, never>, StorageUsage];
  /** The start-up repair of a damaged database, if one happened and wasn't dismissed. */
  "store.recovery": [Record<string, never>, StoreRecovery | null];
  "store.dismissRecovery": [Record<string, never>, null];
  "settings.get": [{ key: string }, JsonValue];
  "settings.set": [{ key: string; value: JsonValue }, null];
  // ---- chat ("Ask Almond", Rust: almond_core::chat) ----
  /** Ask in a thread; a new `thread_id` (client-chosen) or none starts a thread. Answer streams via `chat.delta`. */
  "chat.ask": [ChatAskParams, { thread_id: Id; entry: ChatEntry }];
  /** Threads, most recently updated first. */
  "chat.threads": [{ scope?: ChatScope | null; limit?: number }, ChatThread[]];
  "chat.get": [{ thread_id: Id }, ChatThread];
  "chat.delete": [{ thread_id: Id }, null];
  /** Stop a streaming answer (what was written so far is kept); true if one was running. */
  "chat.cancel": [{ thread_id: Id }, boolean];
  // ---- recipes (cookbook; Rust: almond_core::recipes) ----
  "recipes.list": [Record<string, never>, RecipeInfo[]];
  /** Create (no `id`) or update. Editing a built-in's text saves a user copy (new id); `auto_run` / `hidden` on a built-in are preferences. */
  "recipes.save": [RecipeSaveParams, RecipeInfo];
  /** User recipes only; built-ins can only be hidden (`recipes.save { id, hidden: true }`). */
  "recipes.delete": [{ id: string }, null];
  /** Runs on the meeting's latest transcript; the answer arrives as a `recipe_output` artifact. */
  "recipes.run": [{ recipe_id: string; meeting_id: Id; extra?: string | null; lang?: string | null }, Job];
  /** Live `recipe_output` artifacts of a meeting, latest first, one per recipe. */
  "recipes.outputs": [{ meeting_id: Id }, Artifact[]];
  // ---- people (docs/API.md "People") ----
  "people.list": [Record<string, never>, PersonSummary[]];
  "people.get": [{ id: Id }, PersonDetail];
  "people.save": [SavePersonParams, Person];
  "people.delete": [{ id: Id }, null];
  "people.merge": [{ keep: Id; remove: Id }, Person];
  /** Label a transcript speaker as a person (creates one from `name`; adds a voice print). */
  "people.labelSpeaker": [LabelSpeakerParams, SpeakerLabels];
  "people.unlabelSpeaker": [{ meeting_id: Id; speaker: string }, SpeakerLabels];
  /** Labels the core would suggest now (voice, calendar, "me"); nothing is written. */
  "people.suggestions": [{ meeting_id: Id }, SpeakerLabel[]];
}

/** `store.recovery` (Rust: `almond_store::RecoveryReport`). */
export interface StoreRecovery {
  reason: string;
  backup_dir: string;
  events_recovered: number;
  events_lost: number;
  tables: Record<string, [number, number]>;
}

/** `chat.ask` params. `scope` defaults to `{ type: "all" }` for a new thread. */
export interface ChatAskParams {
  thread_id?: Id | null;
  scope?: ChatScope | null;
  message: string;
  /** Answer language; default: the question's language, else `ui.language`. */
  lang?: "vi" | "en" | null;
}

/** `chat.delta` payload: `text` is the next piece of the answer (append it). */
export interface ChatDelta {
  thread_id: Id;
  entry_id: Id;
  text: string;
}

/** A recipe as `recipes.list` returns it (Rust: `almond_core::recipes::StoredRecipe`). */
export type RecipeInfo = Recipe & { hidden: boolean };

/** `recipes.save` params: only the fields to change (name + prompt are required for a new recipe). */
export interface RecipeSaveParams {
  id?: string | null;
  name?: string;
  description?: string;
  /** Emoji; "" clears it. */
  icon?: string;
  prompt?: string;
  auto_run?: boolean;
  hidden?: boolean;
}

// ---- people ----

/** `people.list` item. Voice prints come without vectors. */
export interface PersonSummary {
  person: Person;
  meeting_count: number;
  talk_time_s: number;
  last_met: number | null;
}

/** One meeting with a person (by speaker labels and/or calendar attendee email). */
export interface PersonMeeting {
  meeting_id: Id;
  title: string;
  started_at: number;
  speakers: string[];
  talk_time_s: number;
  /** Strongest label source; null when linked only as a calendar attendee. */
  source: LabelSource | null;
  attendee: boolean;
}

/** `people.get`. */
export interface PersonDetail {
  person: Person;
  /** Newest first. */
  meetings: PersonMeeting[];
  talk_time_s: number;
  last_met: number | null;
}

/** `people.save`: create (no id) or update; omitted fields keep their value. */
export interface SavePersonParams {
  id?: Id | null;
  name: string;
  emails?: string[] | null;
  aliases?: string[] | null;
  notes?: string | null;
  is_self?: boolean | null;
  /** Prints to keep (matched by meeting_id + speaker + created_at); leaving one out deletes it. */
  voiceprints?: VoicePrint[] | null;
}

/** `people.labelSpeaker`: exactly one of person_id, name, me. */
export interface LabelSpeakerParams {
  meeting_id: Id;
  speaker: string;
  person_id?: Id | null;
  name?: string | null;
  me?: boolean;
}

/** `llm.status` (Rust: `almond_core::processing::api::llm_status`). */
export interface LlmStatus {
  /** The Ollama LLM jobs use now. */
  url: string | null;
  /** `url` is the remote one (setting `llm.remote_ollama_url`, e.g. the home PC over Tailscale). */
  remote: boolean;
  remote_configured: string | null;
  models: string[];
  error: string | null;
  /** Effective `processing.offload` (unset: "prefer_peer" without a GPU). */
  offload: "local" | "prefer_peer";
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
  "chat.delta": ChatDelta;
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
  recipeOutput: "recipe_output",
} as const;

/** Processor names the UI triggers explicitly. */
export const Processors = {
  enhanceNotes: "enhance_notes.llm",
  summarize: "summarize.llm",
} as const;
