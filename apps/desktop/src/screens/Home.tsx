import { type SVGProps, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";
import type { Meeting } from "../gen/Meeting";
import type { SearchHit } from "../gen/SearchHit";
import type { UpcomingEvent } from "../gen/UpcomingEvent";
import { useApi, useLoad, useNotification } from "../api/context";
import { errorMessage } from "../api/errors";
import { useFormat } from "../lib/format";
import { baseName, pickMediaFile, useFileDrop } from "../lib/importMedia";
import { formatClock } from "../lib/text";
import { startNotes } from "../lib/startNotes";
import type { ImportProgress } from "../api/types";
import { useAppState } from "../state/AppState";
import { Button, Card, ErrorNote, Highlighted, Icon, Loading, Pill, SectionTitle } from "../components/ui";
import { AskAlmond } from "../components/chat/AskAlmond";
import type { ChatScope } from "../gen/ChatScope";

const HOME_SCOPE: ChatScope = { type: "all" };

function UploadIcon(p: SVGProps<SVGSVGElement>) {
  return (
    <svg width={18} height={18} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.6} strokeLinecap="round" strokeLinejoin="round" aria-hidden {...p}>
      <path d="M12 15V4M7.5 8.5 12 4l4.5 4.5" />
      <path d="M4 14v5a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-5" />
    </svg>
  );
}

function useNow(intervalMs = 30_000) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(t);
  }, [intervalMs]);
  return now;
}

function Greeting() {
  const { t } = useTranslation();
  const h = new Date().getHours();
  const key = h < 12 ? "home.greetingMorning" : h < 18 ? "home.greetingAfternoon" : "home.greetingEvening";
  return <h1 className="text-2xl font-semibold tracking-tight sm:text-3xl">{t(key)}</h1>;
}

function SearchBox({ onResults }: { onResults: (q: string, hits: SearchHit[] | null) => void }) {
  const { t } = useTranslation();
  const api = useApi();
  const [q, setQ] = useState("");
  useEffect(() => {
    const text = q.trim();
    if (!text) {
      onResults("", null);
      return;
    }
    const h = setTimeout(() => {
      api
        .call("search.query", { text, limit: 20 })
        .then((hits) => onResults(text, hits))
        .catch(() => onResults(text, []));
    }, 180);
    return () => clearTimeout(h);
  }, [q, api, onResults]);
  return (
    <label className="flex items-center gap-3 rounded-2xl border border-line bg-surface px-4 py-3 focus-within:border-accent">
      <Icon.search className="text-faint" />
      <input
        type="search"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        placeholder={t("home.searchPlaceholder")}
        aria-label={t("home.searchPlaceholder")}
        className="w-full bg-transparent text-base outline-none placeholder:text-faint"
      />
    </label>
  );
}

function SearchResults({ q, hits, meetings }: { q: string; hits: SearchHit[]; meetings: Meeting[] }) {
  const { t } = useTranslation();
  const byId = new Map(meetings.map((m) => [m.id, m]));
  return (
    <section aria-label={t("home.searchResults")}>
      <SectionTitle>{t("home.searchResults")}</SectionTitle>
      {hits.length === 0 ? (
        <p className="text-sm text-muted">{t("home.searchEmpty", { q })}</p>
      ) : (
        <ul className="flex flex-col divide-y divide-line rounded-2xl border border-line bg-surface">
          {hits.map((h, i) => (
            <li key={i}>
              <Link
                to={`/meeting/${h.meeting_id}${h.kind === "transcript" && h.segment != null ? `?seg=${h.segment}` : ""}`}
                className="block px-4 py-3 hover:bg-sunken"
              >
                <div className="mb-0.5 flex items-center gap-2 text-xs text-muted">
                  <Pill>{t(`home.hitKind.${h.kind}`, { defaultValue: h.kind })}</Pill>
                  <span className="truncate">{byId.get(h.meeting_id)?.title}</span>
                </div>
                <p className="text-sm">
                  <Highlighted text={h.snippet} />
                </p>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function Upcoming({ events, now }: { events: UpcomingEvent[]; now: number }) {
  const { t } = useTranslation();
  const api = useApi();
  const nav = useNavigate();
  const f = useFormat();
  const { platform, recording } = useAppState();
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const start = async (u: UpcomingEvent) => {
    if (u.meeting_id) return nav(`/meeting/${u.meeting_id}`);
    setBusy(u.event.event_id);
    setErr(null);
    try {
      const id = await startNotes(api, { event: u.event, platform });
      nav(`/meeting/${id}`);
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };

  return (
    <section>
      <SectionTitle>{t("home.comingUp")}</SectionTitle>
      {err && <p className="mb-2 text-sm text-bad">{err}</p>}
      {events.length === 0 ? (
        <Card className="text-sm text-muted">
          {t("home.noUpcoming")}{" "}
          <Link to="/settings" className="text-accent underline-offset-2 hover:underline">
            {t("home.connectCalendar")}
          </Link>
        </Card>
      ) : (
        <ul className="grid gap-3">
          {events.map((u) => {
            const e = u.event;
            const mins = Math.round((e.start - now) / 60_000);
            const soon = mins <= 15;
            return (
              <li key={e.event_id}>
                <Card className="flex flex-col gap-3 sm:flex-row sm:items-center">
                  <div className="w-20 shrink-0 text-sm">
                    <div className="font-medium">{f.time(e.start)}</div>
                    <div className={soon ? "text-accent" : "text-faint"}>{mins <= 0 ? t("home.now") : soon ? t("home.inMinutes", { count: mins }) : f.date(e.start)}</div>
                  </div>
                  <div className="min-w-0 flex-1">
                    <p className="truncate font-medium">{e.title}</p>
                    <div className="mt-0.5 flex flex-wrap items-center gap-3 text-xs text-muted">
                      {e.conference_app && (
                        <span className="inline-flex items-center gap-1">
                          <Icon.video width={14} height={14} />
                          {e.conference_app.replace("_", " ")}
                        </span>
                      )}
                      {e.attendees.length > 0 && (
                        <span className="inline-flex items-center gap-1">
                          <Icon.people width={14} height={14} />
                          {e.attendees
                            .filter((a) => !a.is_self)
                            .map((a) => a.name ?? a.email)
                            .join(", ")}
                        </span>
                      )}
                      {e.location && <span>{e.location}</span>}
                    </div>
                  </div>
                  <Button
                    variant={soon ? "primary" : "secondary"}
                    size="sm"
                    disabled={busy === e.event_id || (!u.meeting_id && recording.state !== "idle")}
                    onClick={() => start(u)}
                    className="self-start sm:self-center"
                  >
                    {u.meeting_id ? t("home.openNotes") : t("home.startNotes")}
                  </Button>
                </Card>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function Recent({ meetings, onImport, importing }: { meetings: Meeting[]; onImport: () => void; importing: boolean }) {
  const { t } = useTranslation();
  const f = useFormat();
  const groups = useMemo(() => {
    const g: { day: string; items: Meeting[] }[] = [];
    for (const m of meetings) {
      const day = f.day(m.started_at);
      const last = g[g.length - 1];
      if (last && last.day === day) last.items.push(m);
      else g.push({ day, items: [m] });
    }
    return g;
  }, [meetings, f]);

  return (
    <section>
      <SectionTitle>{t("home.recent")}</SectionTitle>
      {meetings.length === 0 ? (
        <Card className="flex flex-col items-start gap-3 text-sm text-muted">
          <p>{t("home.noMeetings")}</p>
          <p>{t("import.emptyHint")}</p>
          <Button size="sm" onClick={onImport} disabled={importing}>
            <UploadIcon width={14} height={14} />
            {t("import.button")}
          </Button>
        </Card>
      ) : (
        <div className="flex flex-col gap-5">
          {groups.map((g) => (
            <div key={g.day}>
              <p className="mb-1.5 px-1 text-xs text-faint">{g.day}</p>
              <ul className="flex flex-col divide-y divide-line rounded-2xl border border-line bg-surface">
                {g.items.map((m) => (
                  <li key={m.id}>
                    <Link to={`/meeting/${m.id}`} className="flex items-center gap-3 px-4 py-3 hover:bg-sunken">
                      <div className="min-w-0 flex-1">
                        <p className="truncate font-medium">{m.title || t("meeting.untitled")}</p>
                        <p className="text-xs text-muted">
                          {f.time(m.started_at)} · {t(`meetingKind.${m.kind.type}`)}
                          {m.languages.length > 0 && ` · ${m.languages.join(" / ")}`}
                        </p>
                      </div>
                      {m.ended_at == null && <span className="rec-dot h-2 w-2 rounded-full bg-rec" />}
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}

type ImportState = {
  path: string;
  fileName: string;
  meetingId: string | null;
  progress: number | null;
  processedMs: number;
  stage: ImportProgress["stage"];
  error: unknown;
  cancelling: boolean;
};

/** `meetings.import` with progress; opens the new meeting when it is stored. */
function useImport() {
  const api = useApi();
  const nav = useNavigate();
  const [state, setState] = useState<ImportState | null>(null);
  const current = useRef<string | null>(null);

  useNotification("import.progress", (p) => {
    if (p.path !== current.current) return;
    setState((s) => (s ? { ...s, meetingId: p.meeting_id, progress: p.progress, processedMs: p.processed_ms, stage: p.stage } : s));
  });

  const start = useCallback(
    async (path: string) => {
      if (current.current) return;
      current.current = path;
      setState({ path, fileName: baseName(path), meetingId: null, progress: null, processedMs: 0, stage: "decoding", error: null, cancelling: false });
      try {
        const m = await api.call("meetings.import", { path });
        current.current = null;
        nav(`/meeting/${m.id}`);
      } catch (e) {
        current.current = null;
        setState((s) => (s && !s.cancelling ? { ...s, error: e, stage: "failed" } : null));
      }
    },
    [api, nav],
  );

  const cancel = useCallback(() => {
    setState((s) => {
      if (s?.meetingId) void api.call("meetings.importCancel", { meeting_id: s.meetingId }).catch(() => {});
      return s ? { ...s, cancelling: true } : s;
    });
  }, [api]);

  return { state, start, cancel, dismiss: () => setState(null), busy: state != null && state.error == null };
}

function ImportCard({ s, onCancel, onDismiss }: { s: ImportState; onCancel: () => void; onDismiss: () => void }) {
  const { t } = useTranslation();
  const pct = s.progress != null ? Math.round(s.progress * 100) : null;
  return (
    <Card className="flex flex-col gap-3" >
      <div className="flex items-center gap-3" data-testid="import-card">
        <UploadIcon className="shrink-0 text-accent" />
        <div className="min-w-0 flex-1">
          <p className="truncate font-medium">{s.fileName}</p>
          <p className="text-xs text-muted">
            {s.error
              ? t("import.failed")
              : s.cancelling
                ? t("import.cancelling")
                : s.stage === "storing" || s.stage === "done"
                  ? t("import.storing")
                  : t("import.decoding", { time: formatClock(s.processedMs) })}
          </p>
        </div>
        {s.error ? (
          <Button size="sm" variant="ghost" onClick={onDismiss}>
            {t("import.dismiss")}
          </Button>
        ) : (
          <Button size="sm" variant="ghost" onClick={onCancel} disabled={s.cancelling || !s.meetingId}>
            {t("import.cancel")}
          </Button>
        )}
      </div>
      {s.error ? (
        <ErrorNote error={s.error} />
      ) : (
        <div
          role="progressbar"
          aria-label={t("import.progressLabel", { name: s.fileName })}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={pct ?? undefined}
          className="h-1.5 overflow-hidden rounded-full bg-sunken"
        >
          <div className={pct == null ? "h-full w-1/3 animate-pulse bg-accent" : "h-full bg-accent transition-[width]"} style={pct == null ? undefined : { width: `${pct}%` }} />
        </div>
      )}
    </Card>
  );
}

export function Home() {
  const { t } = useTranslation();
  const api = useApi();
  const nav = useNavigate();
  const now = useNow();
  const { platform, recording } = useAppState();
  const meetings = useLoad(() => api.call("meetings.list", { text: null, from: null, to: null, tag: null, limit: 50, offset: null }), [api]);
  const upcoming = useLoad(() => api.call("calendar.upcoming", { hours: 24 }), [api]);
  const [search, setSearch] = useState<{ q: string; hits: SearchHit[] | null }>({ q: "", hits: null });
  const onResults = useMemo(() => (q: string, hits: SearchHit[] | null) => setSearch({ q, hits }), []);

  // New meetings can arrive from other devices during sync.
  useNotification("artifact.added", () => void meetings.reload());

  const quick = async () => {
    const id = await startNotes(api, { platform });
    nav(`/meeting/${id}`);
  };

  const imp = useImport();
  const [pickError, setPickError] = useState<unknown>(null);
  const importFile = async () => {
    setPickError(null);
    try {
      const path = await pickMediaFile(api, { title: t("import.dialogTitle"), filterName: t("import.filterName") });
      if (path) void imp.start(path);
    } catch (e) {
      setPickError(e);
    }
  };
  const dragging = useFileDrop(api, (path) => void imp.start(path), !imp.busy);

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-8 px-4 py-6 sm:px-8 sm:py-10">
      <div className="flex items-end justify-between gap-4">
        <Greeting />
        <div className="flex flex-wrap justify-end gap-2">
          <Button onClick={importFile} disabled={imp.busy} title={t("import.dropHint")}>
            <UploadIcon width={16} height={16} />
            {t("import.button")}
          </Button>
          <Button variant="primary" onClick={quick} disabled={recording.state !== "idle"}>
            <Icon.plus width={16} height={16} />
            {t("home.quickNote")}
          </Button>
        </div>
      </div>
      {dragging && (
        <div className="pointer-events-none fixed inset-0 z-50 flex items-center justify-center bg-accent/10 p-8" data-testid="drop-overlay">
          <div className="rounded-2xl border-2 border-dashed border-accent bg-surface px-8 py-6 text-center font-medium">{t("import.dropHere")}</div>
        </div>
      )}
      {pickError != null && <ErrorNote error={pickError} />}
      {imp.state && <ImportCard s={imp.state} onCancel={imp.cancel} onDismiss={imp.dismiss} />}
      <SearchBox onResults={onResults} />
      {search.hits ? (
        <SearchResults q={search.q} hits={search.hits} meetings={meetings.data ?? []} />
      ) : (
        <>
          {upcoming.error ? <ErrorNote error={upcoming.error} onRetry={upcoming.reload} /> : upcoming.data ? <Upcoming events={upcoming.data} now={now} /> : <Loading />}
          <AskAlmond
            scope={HOME_SCOPE}
            title={t("chat.title")}
            placeholder={t("chat.placeholder")}
            suggestions={[t("chat.suggestions.today"), t("chat.suggestions.decided"), t("chat.suggestions.prep")]}
          />
          {meetings.error ? <ErrorNote error={meetings.error} onRetry={meetings.reload} /> : meetings.data ? <Recent meetings={meetings.data} onImport={importFile} importing={imp.busy} /> : <Loading />}
        </>
      )}
    </div>
  );
}
