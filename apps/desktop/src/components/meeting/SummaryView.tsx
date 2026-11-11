import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Artifact } from "../../gen/Artifact";
import type { DeviceInfo } from "../../gen/DeviceInfo";
import { useApi } from "../../api/context";
import { errorMessage } from "../../api/errors";
import { Kinds, Processors } from "../../api/types";
import { body, latest, shortModel, verdictsByBullet, versions, worstLabel } from "../../lib/artifacts";
import { CitationChip, VERDICT_STYLE, VerdictBadge } from "../citations";
import { useSpeakerText } from "../../lib/people";
import { Button, cx, Pill, SectionTitle } from "../ui";

function SummaryBody({ summary, artifacts, testId }: { summary: Artifact; artifacts: Artifact[]; testId?: string }) {
  const s = body(summary, "summary");
  const verdicts = useMemo(() => verdictsByBullet(artifacts, summary.id), [artifacts, summary.id]);
  const named = useSpeakerText();
  let flat = 0;
  return (
    <div className="flex flex-col gap-6" data-testid={testId}>
      {s.sections.map((sec, si) => (
        <section key={si}>
          <h3 className="mb-2 text-base font-semibold">{sec.heading}</h3>
          <ul className="flex flex-col gap-2">
            {sec.bullets.map((b, bi) => {
              const idx = flat++;
              const claims = verdicts.get(idx) ?? [];
              const label = worstLabel(claims.flatMap((c) => c.verdicts));
              return (
                <li key={bi} className="flex items-start gap-2" data-verdict={label ?? undefined}>
                  <span className="mt-2.5 h-1 w-1 shrink-0 rounded-full bg-faint" />
                  <div className="min-w-0 flex-1">
                    <span className={cx(label && VERDICT_STYLE[label].underline)}>{named(b.text)}</span>
                    {b.cites.map((c, ci) => (
                      <CitationChip key={ci} span={c} />
                    ))}
                    {claims.length > 0 && (
                      <span className="ml-1.5 align-baseline">
                        <VerdictBadge claims={claims} />
                      </span>
                    )}
                  </div>
                </li>
              );
            })}
          </ul>
        </section>
      ))}
    </div>
  );
}

function deviceName(id: string, devices: DeviceInfo[]) {
  return devices.find((d) => d.id === id)?.name ?? id.slice(0, 10);
}

export function SummaryView({ artifacts, meetingId, devices, transcriptId }: { artifacts: Artifact[]; meetingId: string; devices: DeviceInfo[]; transcriptId: string | null }) {
  const { t } = useTranslation();
  const api = useApi();
  const all = versions(artifacts, Kinds.summary);
  const newest = latest(artifacts, Kinds.summary);
  const [selected, setSelected] = useState<string | null>(null);
  const [compareTo, setCompareTo] = useState<string | null>(null);
  const [rerunModel, setRerunModel] = useState("qwen3.5:9b");
  const [msg, setMsg] = useState<string | null>(null);

  if (!newest) return <p className="text-sm text-muted">{t("meeting.summaryNone")}</p>;
  const current = all.find((a) => a.id === selected) ?? newest;
  const other = compareTo ? all.find((a) => a.id === compareTo) : undefined;
  const label = (a: Artifact) =>
    t("meeting.versionLabel", { n: all.indexOf(a) + 1, model: shortModel(a.provenance.model), device: deviceName(a.device, devices) }) + (a.id === newest.id ? ` (${t("meeting.latest")})` : "");

  const rerun = async () => {
    setMsg(null);
    try {
      await api.call("artifacts.rerun", {
        meeting_id: meetingId,
        processor: Processors.summarize,
        inputs: transcriptId ? [transcriptId] : current.provenance.inputs,
        params: { model: rerunModel },
        allow_offload: true,
      });
      setMsg(t("meeting.rerunQueued"));
    } catch (e) {
      setMsg(errorMessage(e));
    }
  };

  return (
    <div className="@container flex flex-col gap-5">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <label className="flex min-w-0 items-center gap-2">
          <span className="text-muted">{t("meeting.version")}</span>
          <select
            aria-label={t("meeting.version")}
            value={current.id}
            onChange={(e) => setSelected(e.target.value)}
            className="w-full min-w-0 max-w-[18rem] rounded-full border border-line bg-surface px-3 py-1.5 text-sm"
          >
            {all.map((a) => (
              <option key={a.id} value={a.id}>
                {label(a)}
              </option>
            ))}
          </select>
        </label>
        {all.length > 1 && (
          <label className="flex min-w-0 items-center gap-2">
            <span className="text-muted">{t("meeting.compareWith")}</span>
            <select
              aria-label={t("meeting.compareWith")}
              value={compareTo ?? ""}
              onChange={(e) => setCompareTo(e.target.value || null)}
              className="w-full min-w-0 max-w-[18rem] rounded-full border border-line bg-surface px-3 py-1.5 text-sm"
            >
              <option value="">—</option>
              {all
                .filter((a) => a.id !== current.id)
                .map((a) => (
                  <option key={a.id} value={a.id}>
                    {label(a)}
                  </option>
                ))}
            </select>
          </label>
        )}
        <div className="ml-auto flex items-center gap-2">
          <select aria-label={t("meeting.rerunWith")} value={rerunModel} onChange={(e) => setRerunModel(e.target.value)} className="rounded-full border border-line bg-surface px-3 py-1.5 text-sm">
            <option value="qwen3.5:9b">qwen3.5:9b</option>
            <option value="gemma4:e4b">gemma4:e4b</option>
          </select>
          <Button size="sm" onClick={rerun}>
            {t("meeting.rerun")}
          </Button>
        </div>
      </div>
      {msg && <p className="text-sm text-muted">{msg}</p>}

      {other ? (
        <div className="grid gap-6 @2xl:grid-cols-2">
          <div className="min-w-0">
            <div className="mb-3">
              <Pill>{label(current)}</Pill>
            </div>
            <SummaryBody summary={current} artifacts={artifacts} testId="summary-current" />
          </div>
          <div className="min-w-0 border-t border-line pt-5 @2xl:border-t-0 @2xl:border-l @2xl:pt-0 @2xl:pl-6">
            <div className="mb-3">
              <Pill>{label(other)}</Pill>
            </div>
            <SummaryBody summary={other} artifacts={artifacts} testId="summary-compare" />
          </div>
        </div>
      ) : (
        <SummaryBody summary={current} artifacts={artifacts} testId="summary-current" />
      )}
    </div>
  );
}

export function ActionItemsView({ artifacts }: { artifacts: Artifact[] }) {
  const { t } = useTranslation();
  const named = useSpeakerText();
  const a = latest(artifacts, Kinds.actionItems);
  if (!a) return null;
  const items = body(a, "action_items").items;
  if (!items.length) return null;
  return (
    <section>
      <SectionTitle>{t("meeting.actionItems")}</SectionTitle>
      <ul className="flex flex-col gap-2">
        {items.map((it, i) => (
          <li key={i} className="flex items-start gap-3 rounded-xl border border-line bg-surface p-3">
            <input type="checkbox" aria-label={it.text} className="mt-1 h-4 w-4 accent-[var(--accent)]" />
            <div className="min-w-0 flex-1">
              <p>
                {named(it.text)}
                {it.cites.map((c, k) => (
                  <CitationChip key={k} span={c} />
                ))}
              </p>
              <p className="mt-0.5 flex flex-wrap gap-3 text-xs text-muted">
                {it.owner && (
                  <span>
                    {t("meeting.owner")}: {named(it.owner)}
                  </span>
                )}
                {it.due && (
                  <span>
                    {t("meeting.due")}: {it.due}
                  </span>
                )}
              </p>
            </div>
          </li>
        ))}
      </ul>
    </section>
  );
}
