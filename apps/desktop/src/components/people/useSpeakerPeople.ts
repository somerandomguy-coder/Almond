import { useCallback, useEffect, useMemo, useState } from "react";
import type { Artifact } from "../../gen/Artifact";
import type { Person } from "../../gen/Person";
import type { SpeakerLabel } from "../../gen/SpeakerLabel";
import type { SpeakerLabels } from "../../gen/SpeakerLabels";
import type { Transcript } from "../../gen/Transcript";
import { useApi, useNotification } from "../../api/context";
import { currentLabels, SPEAKER_LABELS, type SpeakerInfo } from "../../lib/people";

/**
 * Who the speakers of a meeting's transcript are: the current `speaker_labels` artifact, plus
 * on-demand suggestions when the transcript has no labels yet, resolved against the people
 * catalog. `fallback(id)` names speakers nobody labelled ("You", "S2").
 */
export function useSpeakerPeople(meetingId: string, artifacts: Artifact[], transcriptId: string | null, transcript: Transcript | null, fallback: (id: string) => string) {
  const api = useApi();
  const [people, setPeople] = useState<Person[]>([]);
  const [override, setOverride] = useState<SpeakerLabels | null>(null);
  const [suggested, setSuggested] = useState<SpeakerLabel[]>([]);

  const reloadPeople = useCallback(() => {
    void api
      .call("people.list", {})
      .then((l) => setPeople(l.map((x) => x.person)))
      .catch(() => {});
  }, [api]);
  useEffect(reloadPeople, [reloadPeople]);

  useNotification("artifact.added", (a) => {
    if (a.meeting_id === meetingId && a.kind === SPEAKER_LABELS) setOverride(null);
  });

  const stored = currentLabels(artifacts, transcriptId);
  const labels: SpeakerLabel[] = override && override.transcript === transcriptId ? override.labels : stored;
  const hasLabelsArtifact = artifacts.some((a) => a.kind === SPEAKER_LABELS && (a.body as unknown as SpeakerLabels).transcript === transcriptId);

  // A transcript nobody labelled yet (e.g. people were added later): ask for suggestions.
  useEffect(() => {
    if (!transcriptId || hasLabelsArtifact) return;
    let alive = true;
    void api
      .call("people.suggestions", { meeting_id: meetingId })
      .then((s) => alive && setSuggested(s.map((l) => ({ ...l, source: l.source === "user" ? "voice" : l.source }))))
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [api, meetingId, transcriptId, hasLabelsArtifact]);

  const all = useMemo(() => [...labels, ...suggested.filter((s) => !labels.some((l) => l.speaker === s.speaker))], [labels, suggested]);

  const info = useCallback(
    (sid: string): SpeakerInfo => {
      const label = all.find((l) => l.speaker === sid) ?? null;
      const person = label ? (people.find((p) => p.id === label.person) ?? null) : null;
      const fromTranscript = transcript?.speakers.find((s) => s.id === sid)?.name;
      return {
        name: person?.name ?? fromTranscript ?? fallback(sid),
        label: person ? label : null,
        person,
        suggested: !!person && label?.source !== "user",
      };
    },
    [all, people, transcript, fallback],
  );

  /** Speaker id → name, for ids whose name differs (summary text). */
  const names = useMemo(() => {
    const m = new Map<string, string>();
    for (const s of transcript?.speakers ?? []) {
      const n = info(s.id).name;
      if (n !== s.id) m.set(s.id, n);
    }
    return m;
  }, [transcript, info]);

  const onLabelled = useCallback(
    (l: SpeakerLabels) => {
      setOverride(l);
      setSuggested([]);
      reloadPeople();
    },
    [reloadPeople],
  );

  return { people, info, names, onLabelled };
}
