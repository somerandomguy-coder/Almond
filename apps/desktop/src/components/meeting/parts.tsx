import { useState } from "react";
import { useTranslation } from "react-i18next";
import type { ActionProposal } from "../../gen/ActionProposal";
import type { ActionResult } from "../../gen/ActionResult";
import type { Artifact } from "../../gen/Artifact";
import type { JobProgress } from "../../gen/JobProgress";
import { useApi, useLoad } from "../../api/context";
import { errorMessage } from "../../api/errors";
import { body } from "../../lib/artifacts";
import { formatClock } from "../../lib/text";
import { CitationChip } from "../citations";
import { Button, Card, cx, Icon, Progress, SectionTitle, Spinner } from "../ui";

/** User blocks in normal ink; AI blocks muted with a left rule, so authorship is obvious at a glance. */
export function EnhancedNotesView({ artifact }: { artifact: Artifact }) {
  const { t } = useTranslation();
  const blocks = body(artifact, "enhanced_notes").blocks;
  return (
    <div className="flex flex-col gap-1" data-testid="enhanced-notes">
      <p className="mb-3 flex items-center gap-2 text-xs text-muted">
        <span className="inline-block h-3 w-0.5 rounded bg-ai/50" />
        {t("meeting.aiLegend")}
      </p>
      {blocks.map((b, i) => {
        const ai = b.origin === "ai";
        const cls = cx(ai ? "text-ai border-l-2 border-ai/30 pl-3" : "text-ink", b.level > 0 && "ml-5");
        const cites = b.cites.map((c, k) => <CitationChip key={k} span={c} />);
        if (b.style === "heading")
          return (
            <h3 key={i} data-origin={b.origin} className={cx("mt-4 mb-1 text-base font-semibold", cls)}>
              {b.text}
            </h3>
          );
        if (b.style === "todo")
          return (
            <div key={i} data-origin={b.origin} className={cx("flex items-start gap-2", cls)}>
              <input type="checkbox" aria-label={b.text} className="mt-1.5 h-4 w-4 accent-[var(--accent)]" />
              <span>
                {b.text}
                {cites}
              </span>
            </div>
          );
        if (b.style === "bullet")
          return (
            <div key={i} data-origin={b.origin} className={cx("flex items-start gap-2", cls)}>
              <span className={cx("mt-2.5 h-1 w-1 shrink-0 rounded-full", ai ? "bg-ai" : "bg-ink")} />
              <span>
                {b.text}
                {cites}
              </span>
            </div>
          );
        return (
          <p key={i} data-origin={b.origin} className={cls}>
            {b.text}
            {cites}
          </p>
        );
      })}
    </div>
  );
}

export function ActionsPanel({ meetingId, artifacts }: { meetingId: string; artifacts: Artifact[] }) {
  const { t } = useTranslation();
  const api = useApi();
  const nProposals = artifacts.filter((a) => a.kind === "action_proposals").length;
  const proposals = useLoad(() => api.call("actions.propose", { meeting_id: meetingId }), [api, meetingId, nProposals]);
  const [running, setRunning] = useState<string | null>(null);
  const [results, setResults] = useState<Record<string, ActionResult | { error: string }>>({});
  const [openPreview, setOpenPreview] = useState<string | null>(null);

  const exec = async (p: ActionProposal) => {
    setRunning(p.id);
    try {
      const r = await api.call("actions.execute", { proposal: p });
      setResults((x) => ({ ...x, [p.id]: r }));
    } catch (e) {
      setResults((x) => ({ ...x, [p.id]: { error: errorMessage(e) } }));
    } finally {
      setRunning(null);
    }
  };

  const list = proposals.data?.proposals ?? [];
  return (
    <section>
      <SectionTitle>{t("meeting.actionsTitle")}</SectionTitle>
      {list.length === 0 ? (
        <p className="text-sm text-muted">{t("meeting.actionsNone")}</p>
      ) : (
        <ul className="flex flex-col gap-2">
          {list.map((p) => {
            const r = results[p.id];
            return (
              <li key={p.id}>
                <Card className="flex flex-col gap-2 !p-3">
                  <div className="flex items-center gap-3">
                    <div className="min-w-0 flex-1">
                      <p className="font-medium">{p.title}</p>
                      <code className="text-xs text-faint">{p.action}</code>
                    </div>
                    <Button size="sm" variant="ghost" onClick={() => setOpenPreview(openPreview === p.id ? null : p.id)} aria-expanded={openPreview === p.id}>
                      {t("meeting.preview")}
                    </Button>
                    {r && "ok" in r && r.ok ? (
                      <span className="inline-flex items-center gap-1 text-sm text-ok">
                        <Icon.check width={16} height={16} />
                        {t("meeting.executed")}
                      </span>
                    ) : (
                      <Button size="sm" variant="primary" disabled={running === p.id} onClick={() => exec(p)}>
                        {running === p.id ? <Spinner /> : null}
                        {running === p.id ? t("meeting.executing") : t("meeting.execute")}
                      </Button>
                    )}
                  </div>
                  {openPreview === p.id && <pre className="overflow-x-auto rounded-lg bg-sunken p-3 font-sans text-sm whitespace-pre-wrap">{p.preview}</pre>}
                  {r && "error" in r && <p className="text-sm text-bad">{r.error}</p>}
                  {r && "ok" in r && (
                    <p className="text-sm text-muted">
                      {r.message}{" "}
                      {r.url && (
                        <a href={r.url} target="_blank" rel="noreferrer" className="text-accent underline-offset-2 hover:underline">
                          {t("meeting.openResult")}
                        </a>
                      )}
                    </p>
                  )}
                </Card>
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

export function JobsList({ jobs }: { jobs: JobProgress[] }) {
  const { t } = useTranslation();
  const active = jobs.filter((j) => j.status === "pending" || j.status === "claimed" || j.status === "failed");
  if (!active.length) return null;
  return (
    <div className="rounded-2xl border border-line bg-surface p-3" aria-label={t("meeting.pipeline")}>
      <p className="mb-2 text-xs font-semibold tracking-wider text-muted uppercase">{t("meeting.pipeline")}</p>
      <ul className="flex flex-col gap-2">
        {active.map((j) => (
          <li key={j.job.id} className="text-sm">
            <div className="mb-1 flex items-center gap-2">
              <code className="text-xs">{j.job.processor}</code>
              <span className="truncate text-xs text-faint">{j.message}</span>
              <span className={cx("ml-auto text-xs", j.status === "failed" ? "text-bad" : "text-muted")}>{t(`meeting.jobs.${j.status}`)}</span>
            </div>
            <Progress value={j.progress} label={j.job.processor} />
          </li>
        ))}
      </ul>
    </div>
  );
}

/** Audio scrubber. Real playback needs an audio URL from the core (see contract request). */
export function AudioBar({ positionMs, durationMs, onSeek, available }: { positionMs: number; durationMs: number; onSeek: (ms: number) => void; available: boolean }) {
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-3 rounded-full border border-line bg-surface px-3 py-1.5 text-xs text-muted" title={available ? undefined : t("meeting.noAudio")}>
      <Icon.play width={14} height={14} className={available ? "text-accent" : "text-faint"} />
      <span className="tabular-nums" data-testid="audio-position">
        {formatClock(positionMs)}
      </span>
      <input
        type="range"
        min={0}
        max={Math.max(1, durationMs)}
        value={Math.min(positionMs, durationMs)}
        onChange={(e) => onSeek(Number(e.target.value))}
        aria-label={t("meeting.audioAt")}
        className="h-1 flex-1 accent-[var(--accent)]"
      />
      <span className="tabular-nums">{formatClock(durationMs)}</span>
    </div>
  );
}
