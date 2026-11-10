import { useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams, useSearchParams } from "react-router";
import type { Artifact } from "../gen/Artifact";
import type { DeviceInfo } from "../gen/DeviceInfo";
import type { JobProgress } from "../gen/JobProgress";
import type { MeetingDetail } from "../gen/MeetingDetail";
import type { SpanRef } from "../gen/SpanRef";
import { useApi, useLoad, useNotification } from "../api/context";
import { errorMessage } from "../api/errors";
import { Kinds, Processors } from "../api/types";
import { body, latest, upsertArtifact } from "../lib/artifacts";
import { useFormat } from "../lib/format";
import { openNotesDoc, type NotesDoc } from "../lib/notesDoc";
import { startNotes } from "../lib/startNotes";
import { formatClock } from "../lib/text";
import { useAppState } from "../state/AppState";
import { CitationContext, type CitationCtx } from "../components/citationContext";
import { ActionsPanel, AudioBar, EnhancedNotesView, JobsList } from "../components/meeting/parts";
import { ActionItemsView, SummaryView } from "../components/meeting/SummaryView";
import { NotesEditor } from "../components/NotesEditor";
import { TranscriptPanel, type TranscriptLine } from "../components/TranscriptPanel";
import { Button, cx, ErrorNote, Icon, Loading, Spinner } from "../components/ui";

type Tab = "notes" | "summary" | "actions" | "transcript";

function VuMeter({ levels }: { levels: [string, number][] }) {
  return (
    <span className="flex h-4 items-end gap-0.5" aria-hidden>
      {levels.map(([ch, v]) => (
        <span key={ch} className="w-1 rounded-full bg-rec/70 transition-[height] duration-200" style={{ height: `${Math.max(15, v * 100)}%` }} />
      ))}
    </span>
  );
}

export function MeetingScreen() {
  const { id = "" } = useParams();
  const api = useApi();
  const detail = useLoad(() => api.call("meetings.get", { id }), [api, id]);
  if (detail.error) {
    return (
      <div className="p-6">
        <ErrorNote error={detail.error} onRetry={detail.reload} />
      </div>
    );
  }
  if (!detail.data) return <Loading />;
  return <MeetingView key={id} detail={detail.data} />;
}

function MeetingView({ detail }: { detail: MeetingDetail }) {
  const { t } = useTranslation();
  const api = useApi();
  const nav = useNavigate();
  const f = useFormat();
  const [search] = useSearchParams();
  const { platform, recording } = useAppState();
  const meeting = detail.meeting;
  const id = meeting.id;

  const [title, setTitle] = useState(meeting.title);
  const [artifacts, setArtifacts] = useState<Artifact[]>(detail.artifacts);
  const [live, setLive] = useState<Map<number, TranscriptLine>>(new Map());
  const [jobs, setJobs] = useState<Map<string, JobProgress>>(new Map());
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [notesDoc, setNotesDoc] = useState<NotesDoc | null>(null);
  const [tab, setTab] = useState<Tab>("notes");
  const [notesView, setNotesView] = useState<"mine" | "enhanced">(latest(detail.artifacts, Kinds.enhancedNotes) ? "enhanced" : "mine");
  const [focus, setFocus] = useState<{ idx: number; nonce: number } | null>(null);
  const [audioPos, setAudioPos] = useState(0);
  const [audioSrc, setAudioSrc] = useState<string | null>(null);
  const [enhanceJob, setEnhanceJob] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const isRecording = recording.meeting_id === id && recording.state !== "idle";

  // ---- live data ----
  useNotification("artifact.added", (a) => {
    if (a.meeting_id !== id) return;
    setArtifacts((l) => upsertArtifact(l, a));
    if (a.kind === Kinds.enhancedNotes) {
      setNotesView("enhanced");
      setEnhanceJob(null);
    }
  });
  useNotification("transcript.update", (u) => {
    if (u.meeting_id !== id) return;
    setLive((m) => new Map(m).set(u.update.segment.idx, { segment: u.update.segment, final: u.update.is_final }));
  });
  const audioTracks = meeting.audio.length;
  useEffect(() => {
    if (isRecording || audioTracks === 0) return;
    let cancelled = false;
    api
      .call("audio.url", { meeting_id: id })
      .then((r) => !cancelled && setAudioSrc(r.url))
      .catch(() => !cancelled && setAudioSrc(null));
    return () => {
      cancelled = true;
    };
  }, [api, id, isRecording, audioTracks]);
  useNotification("job.progress", (j) => {
    if (j.job.meeting_id !== id) return;
    setJobs((m) => new Map(m).set(j.job.id, j));
  });
  useEffect(() => {
    void api.call("jobs.list", {}).then((list) => setJobs(new Map(list.filter((j) => j.job.meeting_id === id).map((j) => [j.job.id, j]))));
    void api
      .call("sync.status", {})
      .then((s) => setDevices([s.this_device, ...s.peers.map((p) => p.device)]))
      .catch(() => {});
  }, [api, id]);

  // ---- notes doc (Loro CRDT) ----
  useEffect(() => {
    let doc: NotesDoc | null = null;
    let alive = true;
    void openNotesDoc(api, meeting.notes_doc).then((d) => {
      if (alive) setNotesDoc((doc = d));
      else d.destroy();
    });
    return () => {
      alive = false;
      doc?.destroy();
    };
  }, [api, meeting.notes_doc]);

  // ---- transcript ----
  const transcriptArt = latest(artifacts, Kinds.transcript);
  const transcript = transcriptArt ? body(transcriptArt, "transcript") : null;
  const lines: TranscriptLine[] = useMemo(() => {
    if (isRecording || (!transcript && live.size)) return [...live.values()].sort((a, b) => a.segment.idx - b.segment.idx);
    return transcript ? transcript.segments.map((segment) => ({ segment, final: true })) : [];
  }, [isRecording, transcript, live]);
  const duration = isRecording ? recording.elapsed_ms : (transcript?.segments.at(-1)?.t1_ms ?? 0);

  const speakerName = useCallback(
    (sid: string | null) => {
      if (!sid) return t("meeting.speakerUnknown");
      const sp = transcript?.speakers.find((s) => s.id === sid);
      if (sp?.name) return sp.name;
      return sid === "me" ? t("app.you") : sid;
    },
    [transcript, t],
  );

  const jump = useCallback(
    (span: SpanRef) => {
      const src = artifacts.find((a) => a.id === span.artifact && a.kind === Kinds.transcript);
      const seg = src ? body(src, "transcript").segments.find((s) => s.idx === span.segment) : undefined;
      setFocus((f0) => ({ idx: span.segment, nonce: (f0?.nonce ?? 0) + 1 }));
      setAudioPos(span.time_ms?.[0] ?? seg?.t0_ms ?? 0);
      if (typeof window !== "undefined" && window.innerWidth < 1024) setTab("transcript");
    },
    [artifacts],
  );

  const citationCtx: CitationCtx = useMemo(
    () => ({
      resolve: (span) => {
        const src = artifacts.find((a) => a.id === span.artifact);
        if (src?.kind !== Kinds.transcript) return undefined;
        return body(src, "transcript").segments.find((s) => s.idx === span.segment);
      },
      speakerName,
      jump,
    }),
    [artifacts, speakerName, jump],
  );

  // Deep link from search: ?seg=3
  useEffect(() => {
    const seg = search.get("seg");
    if (seg != null && transcript) {
      const s = transcript.segments.find((x) => x.idx === Number(seg));
      if (s) {
        setFocus({ idx: s.idx, nonce: 1 });
        setAudioPos(s.t0_ms);
      }
    }
    // only on first load
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // ---- actions ----
  const run = async (fn: () => Promise<unknown>) => {
    setErr(null);
    setBusy(true);
    try {
      await fn();
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const stop = () => run(() => api.call("recording.stop", {}));
  const start = () => run(() => startNotes(api, { meetingId: id, platform }));
  const enhance = () =>
    run(async () => {
      const job = await api.call("artifacts.rerun", {
        meeting_id: id,
        processor: Processors.enhanceNotes,
        inputs: transcriptArt ? [transcriptArt.id] : [],
        params: { notes_doc: meeting.notes_doc, user_notes: notesDoc?.text ?? "" },
        allow_offload: true,
      });
      setEnhanceJob(job.id);
    });
  const saveTitle = () => {
    if (title !== meeting.title) void api.call("meetings.update", { id, title, tags: null }).catch((e) => setErr(errorMessage(e)));
  };
  const del = () => {
    if (window.confirm(t("meeting.deleteConfirm"))) void api.call("meetings.delete", { id }).then(() => nav("/"));
  };

  const enhanced = latest(artifacts, Kinds.enhancedNotes);
  const pipelineBusy = [...jobs.values()].some((j) => j.status === "pending" || j.status === "claimed") || recording.state === "finalizing";
  const enhanceRunning = enhanceJob != null && (jobs.get(enhanceJob)?.status ?? "pending") !== "done";
  const hasSummary = artifacts.some((a) => a.kind === Kinds.summary);
  const ev = detail.calendar_event;
  const leftTab: Exclude<Tab, "transcript"> = tab === "transcript" ? "notes" : tab;

  const tabs: { id: Tab; label: string; mobileOnly?: boolean }[] = [
    { id: "notes", label: t("meeting.tabs.notes") },
    { id: "summary", label: t("meeting.tabs.summary") },
    { id: "actions", label: t("meeting.tabs.actions") },
    { id: "transcript", label: t("meeting.tabs.transcript"), mobileOnly: true },
  ];

  return (
    <CitationContext.Provider value={citationCtx}>
      <div className="flex h-full min-h-0 flex-col">
        {/* Header */}
        <header className="border-b border-line px-4 pt-4 pb-0 sm:px-8">
          <div className="mb-2 flex items-center gap-2 text-sm text-muted">
            <Link to="/" className="inline-flex items-center gap-1 rounded-full px-2 py-1 hover:bg-sunken" aria-label={t("meeting.back")}>
              <Icon.back width={16} height={16} />
              <span className="hidden sm:inline">{t("meeting.back")}</span>
            </Link>
            <span className="truncate">{f.dateTime(meeting.started_at)}</span>
            <span>· {t(`meetingKind.${meeting.kind.type}`)}</span>
            {ev && ev.attendees.length > 0 && <span className="hidden sm:inline">· {t("meeting.attendees", { count: ev.attendees.length })}</span>}
          </div>
          <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
            <input
              value={title}
              onChange={(e) => setTitle(e.target.value)}
              onBlur={saveTitle}
              placeholder={t("meeting.titlePlaceholder")}
              aria-label={t("meeting.titlePlaceholder")}
              className="min-w-0 flex-1 bg-transparent text-2xl font-semibold tracking-tight outline-none placeholder:text-faint"
            />
            <div className="flex items-center gap-2">
              {isRecording ? (
                <>
                  <span className="inline-flex items-center gap-2 text-sm font-medium text-rec">
                    <span className="rec-dot h-2 w-2 rounded-full bg-rec" />
                    <span className="tabular-nums">{formatClock(recording.elapsed_ms)}</span>
                    <VuMeter levels={recording.levels} />
                  </span>
                  <Button variant="danger" onClick={stop} disabled={busy}>
                    <Icon.stop width={14} height={14} />
                    {t("meeting.stop")}
                  </Button>
                </>
              ) : !transcriptArt && !pipelineBusy ? (
                <Button variant="primary" onClick={start} disabled={busy || recording.state !== "idle"}>
                  <Icon.mic width={16} height={16} />
                  {t("meeting.startRecording")}
                </Button>
              ) : null}
              {!isRecording && (
                <Button
                  variant={transcriptArt && !enhanced ? "primary" : "secondary"}
                  onClick={enhance}
                  disabled={!transcriptArt || enhanceRunning || busy}
                  title={!transcriptArt ? t("meeting.enhanceNeedsTranscript") : undefined}
                >
                  {enhanceRunning ? <Spinner /> : <Icon.sparkle width={16} height={16} />}
                  {enhanceRunning ? t("meeting.enhancing") : enhanced ? t("meeting.reenhance") : t("meeting.enhance")}
                </Button>
              )}
            </div>
          </div>
          {err && <p className="mt-2 text-sm text-bad">{err}</p>}
          <nav role="tablist" className="mt-3 -mb-px flex gap-1 overflow-x-auto">
            {tabs.map((x) => (
              <button
                key={x.id}
                role="tab"
                type="button"
                aria-selected={tab === x.id}
                onClick={() => setTab(x.id)}
                className={cx(
                  "border-b-2 px-3 py-2 text-sm whitespace-nowrap",
                  x.mobileOnly && "lg:hidden",
                  tab === x.id ? "border-accent font-medium text-ink" : "border-transparent text-muted hover:text-ink",
                )}
              >
                {x.label}
              </button>
            ))}
          </nav>
        </header>

        {/* Body */}
        <div className="grid min-h-0 flex-1 lg:grid-cols-[minmax(0,1fr)_24rem] xl:grid-cols-[minmax(0,1fr)_28rem]">
          <div className={cx("min-h-0 overflow-y-auto px-4 py-6 sm:px-8", tab === "transcript" ? "hidden lg:block" : "block")}>
            <div className="mx-auto flex max-w-2xl flex-col gap-6">
              <JobsList jobs={[...jobs.values()]} />
              {leftTab === "notes" && (
                <>
                  {enhanced && (
                    <div className="inline-flex w-fit rounded-full border border-line bg-surface p-0.5 text-sm" role="group">
                      {(["enhanced", "mine"] as const).map((v) => (
                        <button
                          key={v}
                          type="button"
                          aria-pressed={notesView === v}
                          onClick={() => setNotesView(v)}
                          className={cx("rounded-full px-3 py-1", notesView === v ? "bg-sunken font-medium" : "text-muted")}
                        >
                          {v === "enhanced" ? t("meeting.viewEnhanced") : t("meeting.viewMine")}
                        </button>
                      ))}
                    </div>
                  )}
                  {enhanced && notesView === "enhanced" ? (
                    <EnhancedNotesView artifact={enhanced} />
                  ) : notesDoc ? (
                    <NotesEditor doc={notesDoc} placeholder={t("meeting.notesPlaceholder")} />
                  ) : (
                    <Loading />
                  )}
                </>
              )}
              {leftTab === "summary" && (
                <>
                  <SummaryView artifacts={artifacts} meetingId={id} devices={devices} transcriptId={transcriptArt?.id ?? null} />
                  <ActionItemsView artifacts={artifacts} />
                </>
              )}
              {leftTab === "actions" && (
                <>
                  <ActionsPanel meetingId={id} artifacts={artifacts} />
                  <div className="border-t border-line pt-4">
                    <Button variant="danger" size="sm" onClick={del}>
                      {t("meeting.delete")}
                    </Button>
                  </div>
                </>
              )}
              {leftTab === "notes" && hasSummary && !isRecording && (
                <button type="button" onClick={() => setTab("summary")} className="w-fit text-sm text-accent hover:underline">
                  {t("meeting.tabs.summary")} →
                </button>
              )}
            </div>
          </div>

          <aside className={cx("min-h-0 flex-col border-line bg-bg lg:border-l", tab === "transcript" ? "flex" : "hidden lg:flex")} aria-label={t("meeting.tabs.transcript")}>
            <div className="flex flex-col gap-2 border-b border-line p-3">
              <p className="flex items-center gap-2 text-xs font-semibold tracking-wider text-muted uppercase">
                {isRecording && <span className="rec-dot h-1.5 w-1.5 rounded-full bg-rec" />}
                {isRecording ? t("meeting.transcriptLive") : t("meeting.tabs.transcript")}
              </p>
              {!isRecording && lines.length > 0 && <AudioBar positionMs={audioPos} durationMs={duration} onSeek={setAudioPos} available={meeting.audio.length > 0} src={audioSrc} />}
            </div>
            <div className="min-h-0 flex-1">
              <TranscriptPanel
                lines={lines}
                live={isRecording}
                focus={focus}
                speakerName={speakerName}
                onSeek={(ms) => {
                  setAudioPos(ms);
                }}
              />
            </div>
          </aside>
        </div>
      </div>
    </CitationContext.Provider>
  );
}
