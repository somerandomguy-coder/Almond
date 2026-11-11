import { useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate } from "react-router";
import { useApi, useLoad } from "../api/context";
import { errorMessage } from "../api/errors";
import { useFormat } from "../lib/format";
import { formatTalkTime, initials } from "../lib/people";
import { fold } from "../lib/text";
import { Button, ErrorNote, Icon, Loading, Pill } from "../components/ui";

export function Avatar({ name, self }: { name: string; self?: boolean }) {
  return (
    <span aria-hidden className={self ? "flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-accent text-xs font-semibold text-accent-ink" : "flex h-9 w-9 shrink-0 items-center justify-center rounded-full bg-sunken text-xs font-semibold text-muted"}>
      {initials(name)}
    </span>
  );
}

/** People catalog: everyone you have met, with search; "Add person". */
export function People() {
  const { t } = useTranslation();
  const api = useApi();
  const nav = useNavigate();
  const f = useFormat();
  const list = useLoad(() => api.call("people.list", {}), [api]);
  const [q, setQ] = useState("");
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [err, setErr] = useState<string | null>(null);

  const add = async () => {
    setErr(null);
    try {
      const p = await api.call("people.save", { name });
      nav(`/people/${p.id}`);
    } catch (e) {
      setErr(errorMessage(e));
    }
  };

  const query = fold(q.trim());
  const rows = (list.data ?? []).filter(
    ({ person: p }) => !query || fold(p.name).includes(query) || p.aliases.some((a) => fold(a).includes(query)) || p.emails.some((e) => e.includes(query)),
  );

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 px-4 py-6 sm:px-8 sm:py-10">
      <div className="flex items-center justify-between gap-3">
        <h1 className="text-2xl font-semibold tracking-tight">{t("people.title")}</h1>
        <Button size="sm" onClick={() => setAdding((a) => !a)}>
          <Icon.plus width={14} height={14} />
          {t("people.add")}
        </Button>
      </div>
      {adding && (
        <form
          className="flex gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (name.trim()) void add();
          }}
        >
          <input
            autoFocus
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder={t("people.namePlaceholder")}
            aria-label={t("people.namePlaceholder")}
            className="min-w-0 flex-1 rounded-full border border-line bg-surface px-4 py-2 text-sm outline-none focus:border-accent"
          />
          <Button type="submit" variant="primary" disabled={!name.trim()}>
            {t("app.add")}
          </Button>
        </form>
      )}
      {err && <p className="text-sm text-bad">{err}</p>}
      <label className="flex items-center gap-2 rounded-full border border-line bg-surface px-4 py-2 text-sm">
        <Icon.search width={16} height={16} className="text-muted" />
        <input value={q} onChange={(e) => setQ(e.target.value)} placeholder={t("people.search")} aria-label={t("people.search")} className="min-w-0 flex-1 bg-transparent outline-none" />
      </label>
      {list.error ? (
        <ErrorNote error={list.error} onRetry={list.reload} />
      ) : !list.data ? (
        <Loading />
      ) : rows.length === 0 ? (
        <p className="text-sm text-muted">{list.data.length ? t("people.noMatch") : t("people.empty")}</p>
      ) : (
        <ul className="flex flex-col divide-y divide-line rounded-2xl border border-line bg-surface" data-testid="people-list">
          {rows.map(({ person: p, meeting_count, talk_time_s, last_met }) => (
            <li key={p.id}>
              <Link to={`/people/${p.id}`} className="flex items-center gap-3 px-4 py-3 hover:bg-sunken">
                <Avatar name={p.name} self={p.is_self} />
                <div className="min-w-0 flex-1">
                  <p className="flex items-center gap-2 truncate font-medium">
                    {p.name}
                    {p.is_self && <Pill>{t("people.me")}</Pill>}
                  </p>
                  <p className="truncate text-xs text-muted">{[p.emails[0], t("people.meetingCount", { count: meeting_count }), talk_time_s > 0 ? formatTalkTime(talk_time_s) : null].filter(Boolean).join(" · ")}</p>
                </div>
                {last_met != null && <span className="shrink-0 text-xs text-faint">{t("people.lastMet", { when: f.relative(last_met) })}</span>}
              </Link>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
