// Speaker labels → names for transcripts and summaries (feature "people").
import { createContext, useContext } from "react";
import type { Artifact } from "../gen/Artifact";
import type { Person } from "../gen/Person";
import type { SpeakerLabel } from "../gen/SpeakerLabel";
import type { SpeakerLabels } from "../gen/SpeakerLabels";
import { latest } from "./artifacts";

export const SPEAKER_LABELS = "speaker_labels";

/** Labels of the meeting's current `speaker_labels` artifact that apply to `transcriptId`. */
export function currentLabels(artifacts: Artifact[], transcriptId: string | null | undefined): SpeakerLabel[] {
  const a = latest(artifacts, SPEAKER_LABELS);
  if (!a || !transcriptId) return [];
  const b = a.body as unknown as SpeakerLabels;
  return b.transcript === transcriptId ? b.labels : [];
}

/** What a transcript speaker chip shows. */
export interface SpeakerInfo {
  /** Display name (person, transcript name, or the raw id). */
  name: string;
  label: SpeakerLabel | null;
  person: Person | null;
  /** Suggested (voice/calendar), not confirmed by the user yet. */
  suggested: boolean;
}

/** Raw speaker ids as they appear in text: "S2", "remote:S2", "SPEAKER_1". */
const ID_IN_TEXT = /\b(?:remote:)?S\d+\b/g;

/**
 * Replace speaker ids in AI text ("S2 will send the plan") with names, for ids that resolve
 * to a different name. `names` maps speaker id → name.
 */
export function nameSpeakersInText(text: string, names: Map<string, string>): string {
  if (!names.size) return text;
  return text.replace(ID_IN_TEXT, (m) => names.get(m) ?? names.get(`remote:${m}`) ?? m);
}

/** Speaker id → display name of the meeting being viewed (summaries use it for their text). */
export const SpeakerNamesContext = createContext<Map<string, string>>(new Map());

/** Text with speaker ids replaced by names (identity outside a meeting). */
export function useSpeakerText(): (text: string) => string {
  const names = useContext(SpeakerNamesContext);
  return (text) => nameSpeakersInText(text, names);
}

export function formatTalkTime(s: number): string {
  if (s < 60) return `${Math.round(s)} s`;
  const m = Math.round(s / 60);
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60} min`;
}

/** Initials for an avatar ("Trần Lan" → "TL"). */
export function initials(name: string): string {
  const parts = name.trim().split(/\s+/).filter(Boolean);
  if (!parts.length) return "?";
  return (parts.length === 1 ? parts[0].slice(0, 2) : parts[0][0] + parts[parts.length - 1][0]).toUpperCase();
}
