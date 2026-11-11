import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, useNavigate, useParams } from "react-router";
import type { PersonDetail } from "../api/types";
import { useApi, useLoad } from "../api/context";
import { errorMessage } from "../api/errors";
import { useFormat } from "../lib/format";
import { formatTalkTime } from "../lib/people";
import { Button, Card, ErrorNote, Icon, Loading, Pill, SectionTitle, Toggle } from "../components/ui";
import { Avatar } from "./People";

const splitList = (s: string) =>
  s
    .split(/[,\n]/)
    .map((x) => x.trim())
    .filter(Boolean);

/** One person: details, meetings with them, talk time, voice prints, merge, delete. */
export function PersonScreen() {
  const { id = "" } = useParams();
  const api = useApi();
  const detail = useLoad(() => api.call("people.get", { id }), [api, id]);
  if (detail.error)
    return (
      <div className="p-6">
        <ErrorNote error={detail.error} onRetry={detail.reload} />
      </div>
    );
  if (!detail.data) return <Loading />;
  return <PersonView key={id} detail={detail.data} reload={detail.reload} />;
}

function PersonView({ detail, reload }: { detail: PersonDetail; reload: () => void }) {
  const { t } = useTranslation();
  const api = useApi();
  const nav = useNavigate();
  const f = useFormat();
  const p = detail.person;
  const [name, setName] = useState(p.name);
  const [emails, setEmails] = useState(p.emails.join(", "));
  const [aliases, setAliases] = useState(p.aliases.join(", "));
  const [notes, setNotes] = useState(p.notes);
  const [msg, setMsg] = useState<string | null>(null);
  const [mergeWith, setMergeWith] = useState("");
  const others = useLoad(() => api.call("people.list", {}), [api]);
  useEffect(() => setName(p.name), [p.name]);

  const act = async (fn: () => Promise<unknown>, done?: string) => {
    setMsg(null);
    try {
      await fn();
      if (done) setMsg(done);
      reload();
      others.reload();
    } catch (e) {
      setMsg(errorMessage(e));
    }
  };
  const save = () => act(() => api.call("people.save", { id: p.id, name, emails: splitList(emails), aliases: splitList(aliases), notes }), t("settings.saved"));
  const setSelf = (v: boolean) => act(() => api.call("people.save", { id: p.id, name: p.name, is_self: v }));
  const dropPrint = (i: number) => act(() => api.call("people.save", { id: p.id, name: p.name, voiceprints: p.voiceprints.filter((_, k) => k !== i) }));
  const merge = () => {
    const other = others.data?.find((o) => o.person.id === mergeWith)?.person;
    if (!other || !window.confirm(t("people.mergeConfirm", { other: other.name, name: p.name }))) return;
    void act(() => api.call("people.merge", { keep: p.id, remove: other.id }), t("people.merged", { name: other.name }));
    setMergeWith("");
  };
  const del = () => {
    if (window.confirm(t("people.deleteConfirm", { name: p.name }))) void api.call("people.delete", { id: p.id }).then(() => nav("/people"), (e) => setMsg(errorMessage(e)));
  };

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-6 px-4 py-6 sm:px-8 sm:py-10">
      <div className="flex items-center gap-2 text-sm text-muted">
        <Link to="/people" className="inline-flex items-center gap-1 rounded-full px-2 py-1 hover:bg-sunken">
          <Icon.back width={16} height={16} />
          {t("people.title")}
        </Link>
      </div>
      <div className="flex items-center gap-3">
        <Avatar name={p.name} self={p.is_self} />
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          onBlur={() => name.trim() && name !== p.name && void save()}
          aria-label={t("people.name")}
          className="min-w-0 flex-1 bg-transparent text-2xl font-semibold tracking-tight outline-none"
        />
        {p.is_self && <Pill>{t("people.me")}</Pill>}
      </div>
      <div className="grid grid-cols-3 gap-3 text-center">
        <Card className="!p-3">
          <p className="text-lg font-semibold">{detail.meetings.length}</p>
          <p className="text-xs text-muted">{t("people.meetings")}</p>
        </Card>
        <Card className="!p-3">
          <p className="text-lg font-semibold" data-testid="talk-time">
            {formatTalkTime(detail.talk_time_s)}
          </p>
          <p className="text-xs text-muted">{t("people.talkTime")}</p>
        </Card>
        <Card className="!p-3">
          <p className="text-lg font-semibold">{detail.last_met != null ? f.date(detail.last_met) : "—"}</p>
          <p className="text-xs text-muted">{t("people.lastMetLabel")}</p>
        </Card>
      </div>
      {msg && <p className="text-sm text-muted">{msg}</p>}

      <section>
        <SectionTitle>{t("people.meetings")}</SectionTitle>
        {detail.meetings.length === 0 ? (
          <p className="text-sm text-muted">{t("people.noMeetings")}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-line rounded-2xl border border-line bg-surface">
            {detail.meetings.map((m) => (
              <li key={m.meeting_id}>
                <Link to={`/meeting/${m.meeting_id}`} className="flex items-center gap-3 px-4 py-3 hover:bg-sunken">
                  <div className="min-w-0 flex-1">
                    <p className="truncate font-medium">{m.title || t("meeting.untitled")}</p>
                    <p className="text-xs text-muted">
                      {[f.dateTime(m.started_at), m.talk_time_s > 0 ? t("people.spoke", { time: formatTalkTime(m.talk_time_s) }) : null].filter(Boolean).join(" · ")}
                    </p>
                  </div>
                  {m.source && m.source !== "user" && <Pill>{t(`people.source.${m.source}`)}</Pill>}
                  {m.attendee && !m.source && <Pill>{t("people.attendee")}</Pill>}
                </Link>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="flex flex-col gap-3">
        <SectionTitle>{t("people.details")}</SectionTitle>
        <label className="flex flex-col gap-1 text-sm">
          <span className="text-muted">{t("people.emails")}</span>
          <input value={emails} onChange={(e) => setEmails(e.target.value)} placeholder="lan@example.com" className="rounded-xl border border-line bg-surface px-3 py-2 outline-none focus:border-accent" />
        </label>
        <label className="flex flex-col gap-1 text-sm">
          <span className="text-muted">{t("people.aliases")}</span>
          <input value={aliases} onChange={(e) => setAliases(e.target.value)} className="rounded-xl border border-line bg-surface px-3 py-2 outline-none focus:border-accent" />
        </label>
        <label className="flex flex-col gap-1 text-sm">
          <span className="text-muted">{t("people.notes")}</span>
          <textarea value={notes} onChange={(e) => setNotes(e.target.value)} rows={3} className="rounded-xl border border-line bg-surface px-3 py-2 outline-none focus:border-accent" />
        </label>
        <div className="flex flex-wrap items-center justify-between gap-3">
          <label className="flex items-center gap-3 text-sm">
            <Toggle checked={p.is_self} onChange={(v) => void setSelf(v)} label={t("people.isMe")} />
            {t("people.isMe")}
          </label>
          <Button variant="primary" size="sm" onClick={() => void save()}>
            {t("app.save")}
          </Button>
        </div>
      </section>

      <section>
        <SectionTitle>{t("people.voice")}</SectionTitle>
        <p className="mb-2 text-sm text-muted">{t("people.voiceHint", { count: p.voiceprints.length })}</p>
        {p.voiceprints.length > 0 && (
          <ul className="flex flex-col gap-1">
            {p.voiceprints.map((v, i) => (
              <li key={`${v.meeting_id}-${v.speaker}-${v.created_at}`} className="flex items-center justify-between gap-2 rounded-xl border border-line bg-surface px-3 py-2 text-sm">
                <span className="min-w-0 truncate">
                  {detail.meetings.find((m) => m.meeting_id === v.meeting_id)?.title ?? v.meeting_id.slice(0, 10)} · {v.speaker} · {Math.round(v.speech_s)} s
                </span>
                <Button size="sm" variant="ghost" onClick={() => void dropPrint(i)} aria-label={t("people.deletePrint")} title={t("people.deletePrint")}>
                  <Icon.trash width={14} height={14} />
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>

      <section className="flex flex-col gap-2">
        <SectionTitle>{t("people.merge")}</SectionTitle>
        <p className="text-sm text-muted">{t("people.mergeHint")}</p>
        <div className="flex gap-2">
          <select value={mergeWith} onChange={(e) => setMergeWith(e.target.value)} aria-label={t("people.merge")} className="min-w-0 flex-1 rounded-full border border-line bg-surface px-3 py-1.5 text-sm">
            <option value="">—</option>
            {(others.data ?? [])
              .filter((o) => o.person.id !== p.id)
              .map((o) => (
                <option key={o.person.id} value={o.person.id}>
                  {o.person.name}
                </option>
              ))}
          </select>
          <Button size="sm" onClick={merge} disabled={!mergeWith}>
            {t("people.mergeButton")}
          </Button>
        </div>
      </section>

      <div className="border-t border-line pt-4">
        <Button variant="danger" size="sm" onClick={del}>
          {t("people.delete")}
        </Button>
      </div>
    </div>
  );
}
