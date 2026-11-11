import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { CalendarSourceInfo } from "../gen/CalendarSourceInfo";
import type { JobProgress } from "../gen/JobProgress";
import type { ModelEntry } from "../gen/ModelEntry";
import type { ModelSlot } from "../gen/ModelSlot";
import { useApi, useLoad, useNotification } from "../api/context";
import type { LlmStatus } from "../api/types";
import { errorMessage } from "../api/errors";
import { LANGUAGES, setLanguage, type UiLang } from "../i18n";
import { useFormat } from "../lib/format";
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
  // Setting `captions.language`: language of live captions (unset = the meeting's language).
  const [captions, setCaptions] = useState("");
  useEffect(() => {
    void api
      .call("settings.get", { key: "captions.language" })
      .then((v) => setCaptions(typeof v === "string" ? v : ""))
      .catch(() => {});
  }, [api]);
  const changeCaptions = (v: string) => {
    setCaptions(v);
    void api.call("settings.set", { key: "captions.language", value: v || null }).catch(() => {});
  };
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
        <Row label={t("settings.captionsLang")} hint={t("settings.captionsLangHint")} htmlFor="set-captions">
          <select id="set-captions" className={selectCls} value={captions} onChange={(e) => changeCaptions(e.target.value)}>
            <option value="">{t("settings.captionsAuto")}</option>
            <option value="en">English</option>
            <option value="vi">Tiếng Việt</option>
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
  const [gmail, setGmail] = useState(false);

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
          <Button onClick={() => act("google", () => api.call("calendar.addGoogle", { gmail }))} disabled={busy === "google"}>
            {busy === "google" ? <Spinner /> : <Icon.plus width={16} height={16} />}
            {busy === "google" ? t("settings.addingGoogle") : t("settings.addGoogle")}
          </Button>
          <label className="flex items-center gap-2 text-sm text-muted">
            <input type="checkbox" checked={gmail} onChange={(e) => setGmail(e.target.checked)} />
            {t("settings.gmailToo")}
          </label>
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
        <GoogleClient />
      </Card>
    </section>
  );
}

/** Setting `google.oauth_client`: the user's own Desktop OAuth client (docs/google-setup.md). */
function GoogleClient() {
  const { t } = useTranslation();
  const api = useApi();
  const [id, setId] = useState("");
  const [secret, setSecret] = useState("");
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);
  useEffect(() => {
    void api
      .call("settings.get", { key: "google.oauth_client" })
      .then((v) => {
        if (v && typeof v === "object" && !Array.isArray(v)) {
          const o = v as Record<string, unknown>;
          if (typeof o.client_id === "string") setId(o.client_id);
          if (typeof o.client_secret === "string") setSecret(o.client_secret);
        }
      })
      .catch(() => {});
  }, [api]);
  const submit = (e: FormEvent) => {
    e.preventDefault();
    setMsg(null);
    const value = id.trim() ? { client_id: id.trim(), client_secret: secret.trim() || null } : null;
    void api
      .call("settings.set", { key: "google.oauth_client", value })
      .then(() => setMsg({ ok: true, text: t("settings.saved") }))
      .catch((err) => setMsg({ ok: false, text: errorMessage(err) }));
  };
  return (
    <details className="text-sm">
      <summary className="cursor-pointer text-muted">{t("settings.googleClient")}</summary>
      <form onSubmit={submit} className="mt-2 flex flex-col gap-2">
        <p className="text-xs text-muted">{t("settings.googleClientHint")}</p>
        <input
          value={id}
          onChange={(e) => setId(e.target.value)}
          placeholder="1234-abc.apps.googleusercontent.com"
          aria-label={t("settings.clientId")}
          autoComplete="off"
          spellCheck={false}
          className="min-w-0 rounded-full border border-line bg-surface px-4 py-2 font-mono text-sm outline-none focus:border-accent"
        />
        <input
          value={secret}
          onChange={(e) => setSecret(e.target.value)}
          type="password"
          placeholder={t("settings.clientSecret")}
          aria-label={t("settings.clientSecret")}
          autoComplete="off"
          className="min-w-0 rounded-full border border-line bg-surface px-4 py-2 font-mono text-sm outline-none focus:border-accent"
        />
        <div className="flex items-center gap-3">
          <Button type="submit">{t("app.save")}</Button>
          {msg && (
            <span role="status" className={cx("text-sm", msg.ok ? "text-ok" : "text-bad")}>
              {msg.text}
            </span>
          )}
        </div>
      </form>
    </details>
  );
}

const RETENTION_CHOICES = [0, 7, 30, 90] as const;

function Storage() {
  const { t } = useTranslation();
  const api = useApi();
  const f = useFormat();
  const usage = useLoad(() => api.call("storage.usage", {}), [api]);
  const list = useLoad(() => api.call("storage.meetings", {}), [api]);
  const [retention, setRetention] = useState<number>(0);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [msg, setMsg] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    void api.call("settings.get", { key: "storage.audio_retention_days" }).then((v) => setRetention(typeof v === "number" ? v : 0));
  }, [api]);
  const rows: [string, number][] = usage.data
    ? [
        [t("settings.storageDb"), usage.data.db_bytes],
        [t("settings.storageAudio"), usage.data.audio_bytes],
        [t("settings.storageBlobs"), usage.data.blobs_bytes],
        [t("settings.storageModels"), usage.data.models_bytes],
      ]
    : [];
  const withAudio = (list.data ?? []).filter((m) => m.audio_bytes > 0);
  const reload = () => {
    usage.reload();
    list.reload();
  };
  const changeRetention = (days: number) => {
    setRetention(days);
    void api.call("settings.set", { key: "storage.audio_retention_days", value: days || null });
  };
  const report = (r: { deleted: number; freed_bytes: number; skipped: [string, string][] }) =>
    setMsg(t("storage.freed", { count: r.deleted, size: formatBytes(r.freed_bytes) }) + (r.skipped.length ? ` ${t("storage.skipped", { count: r.skipped.length })}` : ""));
  const deletePicked = async () => {
    if (!picked.size || !window.confirm(t("storage.deleteManyConfirm", { count: picked.size }))) return;
    setBusy(true);
    try {
      report(await api.call("storage.deleteAudio", { meeting_ids: [...picked] }));
      setPicked(new Set());
      reload();
    } catch (e) {
      setMsg(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const cleanup = async () => {
    setBusy(true);
    try {
      report(await api.call("storage.cleanup", {}));
      reload();
    } catch (e) {
      setMsg(errorMessage(e));
    } finally {
      setBusy(false);
    }
  };
  const toggle = (id: string) =>
    setPicked((p) => {
      const n = new Set(p);
      if (n.has(id)) n.delete(id);
      else n.add(id);
      return n;
    });
  return (
    <section>
      <SectionTitle>{t("settings.storage")}</SectionTitle>
      <Card className="flex flex-col gap-4 text-sm">
        <p className="text-muted">{t("settings.storageHint")}</p>
        {usage.error ? (
          <ErrorNote error={usage.error} onRetry={usage.reload} />
        ) : (
          <dl className="grid grid-cols-2 gap-x-6 gap-y-1">
            {rows.map(([k, v]) => (
              <div key={k} className="contents">
                <dt className="text-muted">{k}</dt>
                <dd className="text-right font-mono">{formatBytes(v)}</dd>
              </div>
            ))}
          </dl>
        )}

        <label className="flex flex-col gap-1">
          <span className="font-medium">{t("storage.retention")}</span>
          <span className="text-xs text-muted">{t("storage.retentionHint")}</span>
          <select
            value={retention}
            onChange={(e) => changeRetention(Number(e.target.value))}
            className="w-fit rounded-lg border border-line bg-bg px-2 py-1"
            aria-label={t("storage.retention")}
          >
            {RETENTION_CHOICES.map((d) => (
              <option key={d} value={d}>
                {d === 0 ? t("storage.retentionOff") : t("storage.retentionDays", { count: d })}
              </option>
            ))}
          </select>
        </label>

        <div className="flex flex-col gap-2">
          <div className="flex items-center gap-2">
            <span className="font-medium">{t("storage.recordings")}</span>
            <Button size="sm" variant="ghost" className="ml-auto" onClick={() => void cleanup()} disabled={busy}>
              {t("storage.cleanupNow")}
            </Button>
            <Button size="sm" variant="danger" onClick={() => void deletePicked()} disabled={busy || picked.size === 0}>
              {t("storage.deleteSelected", { count: picked.size })}
            </Button>
          </div>
          {msg && <p className="text-xs text-muted" role="status">{msg}</p>}
          {list.error ? (
            <ErrorNote error={list.error} onRetry={list.reload} />
          ) : withAudio.length === 0 ? (
            <p className="text-xs text-muted">{t("storage.noRecordings")}</p>
          ) : (
            <ul className="flex max-h-72 flex-col overflow-y-auto" aria-label={t("storage.recordings")}>
              {withAudio.map((m) => (
                <li key={m.meeting_id} className="flex items-center gap-2 border-b border-line py-1.5 last:border-0">
                  <input
                    type="checkbox"
                    checked={picked.has(m.meeting_id)}
                    onChange={() => toggle(m.meeting_id)}
                    disabled={m.keep_audio || m.transcribed_at == null}
                    aria-label={m.title || t("recipes.untitled")}
                  />
                  <span className="min-w-0 flex-1 truncate">
                    {m.keep_audio && <span title={t("storage.kept")}>★ </span>}
                    {m.title || t("recipes.untitled")}
                    <span className="ml-2 text-xs text-faint">
                      {f.date(m.started_at)}
                      {m.transcribed_at == null && ` · ${t("storage.notTranscribed")}`}
                    </span>
                  </span>
                  <span className="font-mono text-xs">{formatBytes(m.audio_bytes)}</span>
                </li>
              ))}
            </ul>
          )}
        </div>
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

/** Settings `llm.remote_ollama_url`, `processing.offload`, `processing.offload_wait_min`. */
function Offload() {
  const { t } = useTranslation();
  const api = useApi();
  const [url, setUrl] = useState("");
  const [mode, setMode] = useState("local");
  const [wait, setWait] = useState(20);
  const [status, setStatus] = useState<LlmStatus | null>(null);
  const [checking, setChecking] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    void api.call("settings.get", { key: "llm.remote_ollama_url" }).then((v) => setUrl(typeof v === "string" ? v : "")).catch(() => {});
    // Unset → the device's default (llm.status reports the effective one).
    void api
      .call("settings.get", { key: "processing.offload" })
      .then((v) => (v === "prefer_peer" || v === "local" ? setMode(v) : api.call("llm.status", {}).then((st) => setMode(st.offload))))
      .catch(() => {});
    void api.call("settings.get", { key: "processing.offload_wait_min" }).then((v) => typeof v === "number" && setWait(v)).catch(() => {});
    void api.call("llm.status", {}).then(setStatus).catch(() => {});
  }, [api]);
  const save = (e: FormEvent) => {
    e.preventDefault();
    setErr(null);
    setChecking(true);
    const v = url.trim();
    void api
      .call("settings.set", { key: "llm.remote_ollama_url", value: v || null })
      .then(() => api.call("llm.status", { refresh: true }))
      .then(setStatus)
      .catch((e2) => setErr(errorMessage(e2)))
      .finally(() => setChecking(false));
  };
  const changeMode = (m: string) => {
    setMode(m);
    void api.call("settings.set", { key: "processing.offload", value: m });
  };
  const changeWait = (m: number) => {
    setWait(m);
    void api.call("settings.set", { key: "processing.offload_wait_min", value: m });
  };
  return (
    <section>
      <SectionTitle>{t("settings.offload")}</SectionTitle>
      <Card className="flex flex-col !py-1">
        <p className="pt-3 text-xs text-muted">{t("settings.offloadHint")}</p>
        <form onSubmit={save} className="flex flex-col gap-2 py-3">
          <label htmlFor="set-remote-ollama" className="text-sm font-medium">
            {t("settings.remoteOllama")}
          </label>
          <p className="text-xs text-muted">{t("settings.remoteOllamaHint")}</p>
          <div className="flex flex-wrap items-center gap-2">
            <input
              id="set-remote-ollama"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="100.80.179.59"
              autoComplete="off"
              spellCheck={false}
              className="min-w-0 flex-1 rounded-full border border-line bg-surface px-4 py-2 font-mono text-sm outline-none focus:border-accent"
            />
            <Button type="submit" disabled={checking}>
              {checking ? <Spinner /> : t("app.save")}
            </Button>
          </div>
          {status && (
            <p role="status" className="flex flex-wrap items-center gap-2 text-sm">
              <Pill className={status.remote ? "bg-ok-soft text-ok" : undefined}>{status.remote ? t("settings.llmRemote") : t("settings.llmLocal")}</Pill>
              <span className="font-mono text-xs text-muted">{t("settings.llmUsing", { url: status.url ?? "—" })}</span>
              {status.remote_configured && !status.remote && <span className="text-xs text-bad">{t("settings.llmUnreachable")}</span>}
            </p>
          )}
          {err && <p className="text-sm text-bad">{err}</p>}
        </form>
        <Row label={t("settings.offloadMode")} hint={t("settings.offloadModeHint")} htmlFor="set-offload">
          <select id="set-offload" className={selectCls} value={mode} onChange={(e) => changeMode(e.target.value)}>
            <option value="local">{t("settings.offloadLocal")}</option>
            <option value="prefer_peer">{t("settings.offloadPreferPeer")}</option>
          </select>
        </Row>
        {mode === "prefer_peer" && (
          <Row label={t("settings.offloadWait")} htmlFor="set-offload-wait">
            <select id="set-offload-wait" className={selectCls} value={wait} onChange={(e) => changeWait(Number(e.target.value))}>
              {[5, 10, 20, 30, 60, 120].map((m) => (
                <option key={m} value={m}>
                  {t("settings.minutes", { count: m })}
                </option>
              ))}
            </select>
          </Row>
        )}
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
      <Offload />
      <Storage />
      <section>
        <SectionTitle>{t("settings.about")}</SectionTitle>
        <p className="text-sm text-muted">{t("settings.backend", { kind: api.kind, platform: platform || "…" })}</p>
      </section>
    </div>
  );
}
