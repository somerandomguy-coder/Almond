import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { CalendarSourceInfo } from "../gen/CalendarSourceInfo";
import type { JobProgress } from "../gen/JobProgress";
import type { ModelEntry } from "../gen/ModelEntry";
import type { ModelSlot } from "../gen/ModelSlot";
import { useApi, useLoad, useNotification } from "../api/context";
import { errorMessage } from "../api/errors";
import { LANGUAGES, setLanguage, type UiLang } from "../i18n";
import { formatBytes } from "../lib/text";
import { useAppState, type Theme } from "../state/AppState";
import { Button, Card, cx, ErrorNote, Icon, Loading, Pill, SectionTitle, Spinner } from "../components/ui";

function Row({ label, hint, children, htmlFor }: { label: string; hint?: string; children: ReactNode; htmlFor?: string }) {
  return (
    <div className="flex flex-col gap-2 py-3 sm:flex-row sm:items-center sm:justify-between sm:gap-6">
      <div className="min-w-0">
        <label htmlFor={htmlFor} className="text-sm font-medium">
          {label}
        </label>
        {hint && <p className="text-xs text-muted">{hint}</p>}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}

const selectCls = "rounded-full border border-line bg-surface px-3 py-1.5 text-sm outline-none focus:border-accent";

function General() {
  const { t, i18n } = useTranslation();
  const api = useApi();
  const { theme, setTheme } = useAppState();
  const changeLang = (l: UiLang) => {
    void setLanguage(l);
    void api.call("settings.set", { key: "ui.language", value: l }).catch(() => {});
  };
  return (
    <section>
      <SectionTitle>{t("settings.general")}</SectionTitle>
      <Card className="divide-y divide-line !py-1">
        <Row label={t("settings.language")} htmlFor="set-lang">
          <select id="set-lang" className={selectCls} value={i18n.language} onChange={(e) => changeLang(e.target.value as UiLang)}>
            {LANGUAGES.map((l) => (
              <option key={l.code} value={l.code}>
                {l.label}
              </option>
            ))}
          </select>
        </Row>
        <Row label={t("settings.theme")} htmlFor="set-theme">
          <select id="set-theme" className={selectCls} value={theme} onChange={(e) => setTheme(e.target.value as Theme)}>
            <option value="system">{t("settings.themeSystem")}</option>
            <option value="light">{t("settings.themeLight")}</option>
            <option value="dark">{t("settings.themeDark")}</option>
          </select>
        </Row>
      </Card>
    </section>
  );
}

function Calendars() {
  const { t } = useTranslation();
  const api = useApi();
  const { platform } = useAppState();
  const sources = useLoad(() => api.call("calendar.sources", {}), [api]);
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [ics, setIcs] = useState("");

  const act = async (key: string, fn: () => Promise<unknown>) => {
    setBusy(key);
    setErr(null);
    try {
      await fn();
      await sources.reload();
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setBusy(null);
    }
  };
  const addIcs = (e: FormEvent) => {
    e.preventDefault();
    const url = ics.trim();
    if (!url) return;
    void act("ics", async () => {
      await api.call("calendar.addIcs", { url });
      setIcs("");
    });
  };
  const list: CalendarSourceInfo[] = sources.data?.sources ?? [];

  return (
    <section>
      <SectionTitle>{t("settings.calendars")}</SectionTitle>
      <Card className="flex flex-col gap-4">
        <p className="text-sm text-muted">{t("settings.calendarsHint")}</p>
        {sources.error ? (
          <ErrorNote error={sources.error} onRetry={sources.reload} />
        ) : list.length === 0 ? (
          <p className="text-sm text-muted">{t("settings.noSources")}</p>
        ) : (
          <ul className="flex flex-col divide-y divide-line">
            {list.map((s) => (
              <li key={s.id} className="flex items-center gap-3 py-2">
                <Icon.calendar className="text-muted" />
                <div className="min-w-0 flex-1">
                  <p className="truncate text-sm font-medium">{s.label}</p>
                  <p className="text-xs text-muted">{t(`settings.provider.${s.provider}`, { defaultValue: s.provider })}</p>
                </div>
                <Button size="sm" variant="ghost" disabled={busy === s.id} onClick={() => act(s.id, () => api.call("calendar.remove", { source: s.id }))}>
                  {t("app.remove")}
                </Button>
              </li>
            ))}
          </ul>
        )}
        <div className="flex flex-wrap gap-2">
          <Button onClick={() => act("google", () => api.call("calendar.addGoogle", {}))} disabled={busy === "google"}>
            {busy === "google" ? <Spinner /> : <Icon.plus width={16} height={16} />}
            {busy === "google" ? t("settings.addingGoogle") : t("settings.addGoogle")}
          </Button>
          {platform === "android" && (
            <Button onClick={() => act("phone", () => api.call("calendar.importPhone", {}))} disabled={busy === "phone"}>
              <Icon.phone width={16} height={16} />
              {t("settings.importPhone")}
            </Button>
          )}
        </div>
        <form onSubmit={addIcs} className="flex flex-col gap-2 sm:flex-row sm:items-center">
          <label htmlFor="ics-url" className="text-sm font-medium sm:w-40">
            {t("settings.icsUrl")}
          </label>
          <div className="flex flex-1 gap-2">
            <input
              id="ics-url"
              type="url"
              value={ics}
              onChange={(e) => setIcs(e.target.value)}
              placeholder={t("settings.icsPlaceholder")}
              className="min-w-0 flex-1 rounded-full border border-line bg-surface px-4 py-2 text-sm outline-none focus:border-accent"
            />
            <Button type="submit" disabled={!ics.trim() || busy === "ics"}>
              {t("app.add")}
            </Button>
          </div>
        </form>
        {err && <p className="text-sm text-bad">{err}</p>}
      </Card>
    </section>
  );
}

function Reminders() {
  const { t } = useTranslation();
  const api = useApi();
  const [lead, setLead] = useState<number | null>(null);
  useEffect(() => {
    void api.call("settings.get", { key: "calendar.reminder_lead_min" }).then((v) => setLead(typeof v === "number" ? v : 5));
  }, [api]);
  const change = (v: number) => {
    setLead(v);
    void api.call("settings.set", { key: "calendar.reminder_lead_min", value: v });
  };
  return (
    <section>
      <SectionTitle>{t("settings.reminders")}</SectionTitle>
      <Card className="!py-1">
        <Row label={t("settings.reminderLead")} htmlFor="set-lead">
          <select id="set-lead" className={selectCls} value={lead ?? 5} onChange={(e) => change(Number(e.target.value))}>
            {[1, 2, 5, 10, 15].map((m) => (
              <option key={m} value={m}>
                {t("settings.minutes", { count: m })}
              </option>
            ))}
          </select>
        </Row>
      </Card>
    </section>
  );
}

const TASKS: { task: string; kind: string; langs: (string | null)[] }[] = [
  { task: "stt", kind: "stt", langs: ["vi", "en"] },
  { task: "stt_live", kind: "stt", langs: ["vi"] },
  { task: "llm_summary", kind: "llm", langs: [null] },
  { task: "llm_verify", kind: "llm", langs: [null] },
  { task: "embed", kind: "embed", langs: [null] },
];

function Models() {
  const { t } = useTranslation();
  const api = useApi();
  const models = useLoad(() => api.call("models.list", {}), [api]);
  const [slots, setSlots] = useState<ModelSlot[]>([]);
  const [downloads, setDownloads] = useState<Record<string, JobProgress>>({});

  useEffect(() => {
    // Effective selection: the user's overrides plus this device's defaults.
    void api.call("models.slots", {}).then(setSlots);
  }, [api]);
  useNotification("job.progress", (j) => {
    if (j.job.processor !== "models.download") return;
    const id = (j.job.params as { id?: string } | null)?.id;
    if (!id) return;
    setDownloads((d) => ({ ...d, [id]: j }));
    if (j.status === "done") void models.reload();
  });

  const select = (task: string, lang: string | null, model: string) => {
    const slot = { task, lang, model };
    setSlots((s) => [...s.filter((x) => !(x.task === task && x.lang === lang)), slot]);
    void api.call("models.select", slot);
  };
  const download = (m: ModelEntry) => void api.call("models.download", { id: m.id });

  if (models.error) return <ErrorNote error={models.error} onRetry={models.reload} />;
  if (!models.data) return <Loading />;
  const list = models.data;

  return (
    <section>
      <SectionTitle>{t("settings.models")}</SectionTitle>
      <Card className="flex flex-col gap-1">
        <p className="mb-2 text-sm text-muted">{t("settings.modelsHint")}</p>
        <div className="divide-y divide-line">
          {TASKS.flatMap(({ task, kind, langs }) =>
            langs.map((lang) => {
              const options = list.filter((m) => m.kind === kind && m.installed && (!lang || m.languages.length === 0 || m.languages.includes(lang)));
              const cur = slots.find((s) => s.task === task && s.lang === lang)?.model ?? "";
              const idAttr = `slot-${task}-${lang ?? "all"}`;
              return (
                <Row key={idAttr} htmlFor={idAttr} label={`${t(`settings.task.${task}`)} · ${lang ? t(`settings.lang.${lang}`) : t("settings.allLanguages")}`}>
                  <select id={idAttr} className={cx(selectCls, "max-w-[16rem]")} value={cur} onChange={(e) => select(task, lang, e.target.value)}>
                    {!cur && <option value="">—</option>}
                    {cur && !options.some((m) => m.id === cur) && <option value={cur}>{cur}</option>}
                    {options.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.id}
                      </option>
                    ))}
                  </select>
                </Row>
              );
            }),
          )}
        </div>
        <ul className="mt-3 flex flex-col divide-y divide-line border-t border-line">
          {list.map((m) => {
            const d = downloads[m.id];
            const downloading = d && d.status !== "done" && d.status !== "failed";
            return (
              <li key={m.id} className="flex items-center gap-3 py-2">
                <div className="min-w-0 flex-1">
                  <p className="truncate font-mono text-sm">{m.id}</p>
                  <p className="text-xs text-muted">
                    {m.kind} · {formatBytes(m.size_bytes)}
                    {m.languages.length > 0 && ` · ${m.languages.join(", ")}`}
                    {m.note && ` · ${m.note}`}
                  </p>
                </div>
                {m.installed ? (
                  <Pill className="bg-ok-soft text-ok">{t("settings.installed")}</Pill>
                ) : downloading ? (
                  <span className="text-xs text-muted">{t("settings.downloading", { pct: Math.round(d.progress * 100) })}</span>
                ) : (
                  <Button size="sm" onClick={() => download(m)}>
                    {t("settings.download")}
                  </Button>
                )}
              </li>
            );
          })}
        </ul>
      </Card>
    </section>
  );
}

export function Settings() {
  const { t } = useTranslation();
  const api = useApi();
  const { platform } = useAppState();
  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-8 px-4 py-6 sm:px-8 sm:py-10">
      <h1 className="text-2xl font-semibold tracking-tight">{t("settings.title")}</h1>
      <General />
      <Calendars />
      <Reminders />
      <Models />
      <section>
        <SectionTitle>{t("settings.storage")}</SectionTitle>
        <Card className="text-sm text-muted">{t("settings.storageHint")}</Card>
      </section>
      <section>
        <SectionTitle>{t("settings.about")}</SectionTitle>
        <p className="text-sm text-muted">{t("settings.backend", { kind: api.kind, platform: platform || "…" })}</p>
      </section>
    </div>
  );
}
