import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import type { Person } from "../../gen/Person";
import type { SpeakerLabels } from "../../gen/SpeakerLabels";
import { useApi } from "../../api/context";
import { errorMessage } from "../../api/errors";
import { fold } from "../../lib/text";
import type { SpeakerInfo } from "../../lib/people";
import { cx, Icon, Spinner } from "../ui";

/**
 * A transcript speaker's name. Click â†’ "Who is this?": existing people, a new name, or "this is
 * me". A suggested label (voice / calendar) shows dimmed with a confirm tick.
 */
export function SpeakerChip({
  meetingId,
  speaker,
  info,
  people,
  colorClass,
  onLabelled,
}: {
  meetingId: string;
  speaker: string;
  info: SpeakerInfo;
  people: Person[];
  colorClass?: string;
  onLabelled: (labels: SpeakerLabels) => void;
}) {
  const { t } = useTranslation();
  const api = useApi();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [q, setQ] = useState("");
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (box.current && !box.current.contains(e.target as Node)) setOpen(false);
    };
    const esc = (e: KeyboardEvent) => e.key === "Escape" && setOpen(false);
    document.addEventListener("mousedown", close);
    document.addEventListener("keydown", esc);
    return () => {
      document.removeEventListener("mousedown", close);
      document.removeEventListener("keydown", esc);
    };
  }, [open]);

  const run = async (fn: () => Promise<SpeakerLabels>) => {
    setBusy(true);
    setErr(null);
    try {
      onLabelled(await fn());
      setOpen(false);
      setQ("");
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const label = (p: { person_id?: string; name?: string; me?: boolean }) => run(() => api.call("people.labelSpeaker", { meeting_id: meetingId, speaker, ...p }));
  const unlabel = () => run(() => api.call("people.unlabelSpeaker", { meeting_id: meetingId, speaker }));

  const query = fold(q.trim());
  const matches = people.filter((p) => !query || fold(p.name).includes(query) || p.aliases.some((a) => fold(a).includes(query)) || p.emails.some((e) => e.includes(query)));
  const exact = people.some((p) => fold(p.name) === query);
  const self = people.find((p) => p.is_self);
  const pct = info.label?.confidence != null ? Math.round(info.label.confidence * 100) : null;

  return (
    <span ref={box} className="relative inline-flex items-center gap-1">
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-haspopup="dialog"
        aria-expanded={open}
        title={info.suggested ? t("people.suggestedTitle", { source: t(`people.source.${info.label?.source ?? "voice"}`), pct: pct ?? "â€“" }) : t("people.whoIsThis")}
        data-speaker={speaker}
        data-suggested={info.suggested ? "true" : undefined}
        className={cx("rounded font-medium hover:underline", colorClass, info.suggested && "italic opacity-60")}
      >
        {info.name}
      </button>
      {info.suggested && info.label && (
        <button
          type="button"
          disabled={busy}
          onClick={() => label({ person_id: info.label!.person })}
          aria-label={t("people.confirm", { name: info.name })}
          title={t("people.confirm", { name: info.name })}
          className="rounded-full p-0.5 text-muted hover:bg-sunken hover:text-ok"
        >
          <Icon.check width={12} height={12} />
        </button>
      )}
      {open && (
        <div role="dialog" aria-label={t("people.whoIsThis")} className="absolute top-6 left-0 z-50 w-72 rounded-2xl border border-line bg-surface p-3 text-sm text-ink shadow-lg">
          <p className="mb-2 font-semibold">{t("people.whoIsThis")}</p>
          {info.suggested && info.label && (
            <div className="mb-2 flex items-center justify-between gap-2 rounded-xl bg-sunken px-2 py-1.5">
              <span className="min-w-0 truncate">
                {t("people.suggestion", { name: info.name })}
                {pct != null && <span className="ml-1 text-xs text-muted">{pct}%</span>}
              </span>
              <button type="button" disabled={busy} onClick={() => label({ person_id: info.label!.person })} className="shrink-0 rounded-full bg-accent px-2 py-0.5 text-xs font-medium text-accent-ink">
                {t("people.confirmShort")}
              </button>
            </div>
          )}
          <input
            autoFocus
            value={q}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && q.trim()) {
                const hit = people.find((p) => fold(p.name) === query);
                void label(hit ? { person_id: hit.id } : { name: q.trim() });
              }
            }}
            placeholder={t("people.namePlaceholder")}
            aria-label={t("people.namePlaceholder")}
            className="mb-2 w-full rounded-xl border border-line bg-bg px-3 py-1.5 outline-none focus:border-accent"
          />
          <ul className="flex max-h-48 flex-col overflow-y-auto">
            {matches.slice(0, 8).map((p) => (
              <li key={p.id}>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => label({ person_id: p.id })}
                  className={cx("flex w-full items-center justify-between rounded-lg px-2 py-1.5 text-left hover:bg-sunken", info.label?.person === p.id && !info.suggested && "font-medium")}
                >
                  <span className="truncate">{p.name}</span>
                  {p.is_self && <span className="text-xs text-muted">{t("people.me")}</span>}
                </button>
              </li>
            ))}
            {q.trim() && !exact && (
              <li>
                <button type="button" disabled={busy} onClick={() => label({ name: q.trim() })} className="flex w-full items-center gap-2 rounded-lg px-2 py-1.5 text-left text-accent hover:bg-sunken">
                  <Icon.plus width={14} height={14} />
                  {t("people.addNamed", { name: q.trim() })}
                </button>
              </li>
            )}
          </ul>
          <div className="mt-2 flex flex-wrap items-center gap-2 border-t border-line pt-2">
            {!self?.id || info.label?.person !== self.id || info.suggested ? (
              <button type="button" disabled={busy} onClick={() => label({ me: true })} className="rounded-full border border-line px-2.5 py-1 text-xs hover:bg-sunken">
                {t("people.thisIsMe")}
              </button>
            ) : null}
            {info.label && (
              <button type="button" disabled={busy} onClick={unlabel} className="rounded-full px-2.5 py-1 text-xs text-muted hover:bg-sunken hover:text-bad">
                {t("people.removeLabel")}
              </button>
            )}
            {busy && <Spinner className="ml-auto" />}
          </div>
          {err && <p className="mt-2 text-xs text-bad">{err}</p>}
        </div>
      )}
    </span>
  );
}
