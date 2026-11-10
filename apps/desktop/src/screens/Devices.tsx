import QRCode from "qrcode";
import { useEffect, useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import type { PairBeginResult } from "../gen/PairBeginResult";
import type { PeerInfo } from "../gen/PeerInfo";
import type { SyncProgress } from "../gen/SyncProgress";
import type { SyncReport } from "../gen/SyncReport";
import { useApi, useLoad, useNotification } from "../api/context";
import { errorMessage } from "../api/errors";
import { useFormat } from "../lib/format";
import { formatClock } from "../lib/text";
import { Button, Card, cx, ErrorNote, Icon, Loading, Pill, PlatformIcon, Progress, SectionTitle, Spinner, Toggle } from "../components/ui";

function phaseValue(p: SyncProgress): number {
  if (p.phase === "done") return 1;
  if (p.phase === "connecting") return 0.05;
  const frac = p.total ? p.done / p.total : 0;
  return p.phase === "events" ? 0.1 + 0.6 * frac : 0.7 + 0.3 * frac;
}

function PairingCode() {
  const { t } = useTranslation();
  const api = useApi();
  const [res, setRes] = useState<PairBeginResult | null>(null);
  const [qr, setQr] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now());
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    if (!res) return;
    const tm = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(tm);
  }, [res]);

  const begin = async () => {
    setErr(null);
    try {
      const r = await api.call("sync.pairBegin", {});
      setRes(r);
      setNow(Date.now());
      setQr(await QRCode.toDataURL(r.qr_payload, { margin: 1, width: 220, color: { dark: "#1d1c1a", light: "#ffffff" } }).catch(() => null));
    } catch (e) {
      setErr(errorMessage(e));
    }
  };

  const left = res ? res.expires_at - now : 0;
  return (
    <Card className="flex flex-col gap-3">
      <p className="font-medium">{t("devices.showCode")}</p>
      <p className="text-sm text-muted">{t("devices.showCodeHint")}</p>
      {!res || left <= 0 ? (
        <div className="flex items-center gap-3">
          <Button onClick={begin}>{t("devices.showCode")}</Button>
          {res && left <= 0 && <span className="text-sm text-muted">{t("devices.expired")}</span>}
        </div>
      ) : (
        <div className="flex flex-col items-center gap-3 sm:flex-row sm:items-start">
          {qr ? <img src={qr} alt="QR" width={180} height={180} className="rounded-xl border border-line bg-white p-1" /> : <code className="max-w-full break-all text-xs">{res.qr_payload}</code>}
          <div className="flex flex-col gap-2">
            <ol className="grid grid-cols-3 gap-1.5" aria-label="pairing words">
              {res.words.map((w, i) => (
                <li key={i} className="rounded-lg bg-sunken px-2 py-1 text-center font-mono text-sm">
                  <span className="mr-1 text-faint">{i + 1}</span>
                  {w}
                </li>
              ))}
            </ol>
            <p className="text-xs text-muted">{t("devices.expiresIn", { time: formatClock(left) })}</p>
          </div>
        </div>
      )}
      {err && <p className="text-sm text-bad">{err}</p>}
    </Card>
  );
}

function EnterCode({ onPaired }: { onPaired: (p: PeerInfo) => void }) {
  const { t } = useTranslation();
  const api = useApi();
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const words = text.trim().toLowerCase().split(/[\s,-]+/).filter(Boolean);
    if (words.length !== 6) return setMsg({ ok: false, text: t("errors.invalidWords") });
    setBusy(true);
    setMsg(null);
    try {
      const p = await api.call("sync.pairAccept", { words });
      setMsg({ ok: true, text: t("devices.paired", { name: p.device.name }) });
      setText("");
      onPaired(p);
    } catch (err) {
      setMsg({ ok: false, text: errorMessage(err) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <form onSubmit={submit} className="flex flex-col gap-3">
        <label htmlFor="pair-words" className="font-medium">
          {t("devices.enterCode")}
        </label>
        <p className="text-sm text-muted">{t("devices.enterCodeHint")}</p>
        <div className="flex flex-col gap-2 sm:flex-row">
          <input
            id="pair-words"
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder={t("devices.wordsPlaceholder")}
            autoComplete="off"
            autoCapitalize="none"
            spellCheck={false}
            className="min-w-0 flex-1 rounded-full border border-line bg-surface px-4 py-2 font-mono text-sm outline-none focus:border-accent"
          />
          <Button type="submit" variant="primary" disabled={busy || !text.trim()}>
            {busy ? <Spinner /> : null}
            {busy ? t("devices.pairing") : t("devices.pairButton")}
          </Button>
        </div>
        {msg && (
          <p role="status" className={cx("text-sm", msg.ok ? "text-ok" : "text-bad")}>
            {msg.text}
          </p>
        )}
      </form>
    </Card>
  );
}

export function Devices() {
  const { t } = useTranslation();
  const api = useApi();
  const f = useFormat();
  const status = useLoad(() => api.call("sync.status", {}), [api]);
  const [progress, setProgress] = useState<Record<string, SyncProgress>>({});
  const [syncing, setSyncing] = useState(false);
  const [reports, setReports] = useState<SyncReport[] | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [auto, setAuto] = useState<boolean | null>(null);
  const [addr, setAddr] = useState("");

  useNotification("sync.progress", (p) => setProgress((m) => ({ ...m, [p.peer]: p })));
  useEffect(() => {
    if (status.data) {
      setAuto((a) => a ?? status.data!.auto_sync_while_open);
      if (status.data.syncing) setSyncing(true);
    }
  }, [status.data]);

  const syncNow = async () => {
    setErr(null);
    setSyncing(true);
    setProgress({});
    try {
      const r = await api.call("sync.now", {});
      setReports(r);
      await status.reload();
    } catch (e) {
      setErr(errorMessage(e));
    } finally {
      setSyncing(false);
    }
  };

  const toggleAuto = (v: boolean) => {
    setAuto(v);
    void api.call("settings.set", { key: "sync.auto_while_open", value: v }).catch((e) => setErr(errorMessage(e)));
  };

  const unpair = async (p: PeerInfo) => {
    if (!window.confirm(t("devices.unpairConfirm", { name: p.device.name }))) return;
    await api.call("sync.unpair", { device: p.device.id });
    void status.reload();
  };

  if (status.error) return <div className="p-6"><ErrorNote error={status.error} onRetry={status.reload} /></div>;
  if (!status.data) return <Loading />;
  const s = status.data;
  const shownReports = reports ?? s.last_reports;

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-8 px-4 py-6 sm:px-8 sm:py-10">
      <h1 className="text-2xl font-semibold tracking-tight">{t("devices.title")}</h1>

      {/* Sync hero */}
      <Card className="flex flex-col items-center gap-4 py-8 text-center">
        <Button variant="primary" size="lg" onClick={syncNow} disabled={syncing} className="min-w-48">
          <Icon.sync className={syncing ? "animate-spin" : ""} />
          {syncing ? t("devices.syncing") : t("devices.syncNow")}
        </Button>
        <p className="max-w-md text-sm text-muted">{t("devices.syncHint")}</p>
        {err && <p className="text-sm text-bad">{err}</p>}
        <div className="flex w-full max-w-md items-start justify-between gap-4 border-t border-line pt-4 text-left">
          <div>
            <label htmlFor="auto-sync" className="text-sm font-medium">
              {t("devices.autoSync")}
            </label>
            <p className="text-xs text-muted">{t("devices.autoSyncHint")}</p>
          </div>
          <Toggle id="auto-sync" checked={!!auto} onChange={toggleAuto} label={t("devices.autoSync")} />
        </div>
      </Card>

      {/* This device */}
      <section>
        <SectionTitle>{t("devices.thisDevice")}</SectionTitle>
        <Card className="flex items-center gap-4">
          <span className="rounded-xl bg-accent-soft p-3 text-accent">
            <PlatformIcon platform={s.this_device.platform} width={22} height={22} />
          </span>
          <div className="min-w-0 flex-1">
            <p className="font-medium">{s.this_device.name}</p>
            <p className="text-xs text-muted">
              {t(`devices.platform.${s.this_device.platform}`, { defaultValue: s.this_device.platform })} · v{s.this_device.app_version} · <code>{s.this_device.id.slice(0, 16)}</code>
            </p>
            <div className="mt-2 flex flex-wrap gap-1" aria-label={t("devices.capabilities")}>
              {s.this_device.capabilities.map((c) => (
                <Pill key={c}>{c}</Pill>
              ))}
            </div>
          </div>
        </Card>
      </section>

      {/* Peers */}
      <section>
        <SectionTitle>{t("devices.peers")}</SectionTitle>
        {s.peers.length === 0 ? (
          <p className="text-sm text-muted">{t("devices.noPeers")}</p>
        ) : (
          <ul className="flex flex-col gap-3">
            {s.peers.map((p) => {
              const pr = progress[p.device.id];
              const rep = shownReports.find((r) => r.peer === p.device.id);
              return (
                <li key={p.device.id}>
                  <Card className="flex flex-col gap-3" >
                    <div className="flex items-center gap-4" data-testid={`peer-${p.device.name}`}>
                      <span className="rounded-xl bg-sunken p-3 text-muted">
                        <PlatformIcon platform={p.device.platform} width={22} height={22} />
                      </span>
                      <div className="min-w-0 flex-1">
                        <p className="font-medium">{p.device.name}</p>
                        <p className="text-xs text-muted">
                          {p.addrs[0] ?? "—"}
                          {p.last_seen != null && ` · ${t("devices.lastSeen", { when: f.relative(p.last_seen) })}`}
                        </p>
                        <p className="text-xs text-muted">{p.last_synced != null ? t("devices.lastSynced", { when: f.relative(p.last_synced) }) : t("devices.neverSynced")}</p>
                      </div>
                      <Button variant="ghost" size="sm" onClick={() => unpair(p)}>
                        {t("devices.unpair")}
                      </Button>
                    </div>
                    {pr && (syncing || pr.phase !== "done") && (
                      <div className="flex items-center gap-3">
                        <Progress value={phaseValue(pr)} label={p.device.name} />
                        <span className="w-28 shrink-0 text-right text-xs text-muted">
                          {t(`devices.phase.${pr.phase}`, { defaultValue: pr.phase })}
                          {pr.total > 0 && pr.phase !== "done" ? ` ${pr.done}/${pr.total}` : ""}
                        </span>
                      </div>
                    )}
                    {rep && !syncing && (
                      <p className={cx("text-xs", rep.error ? "text-bad" : "text-muted")}>
                        {rep.error ?? t("devices.report", { sent: rep.events_sent, received: rep.events_received, blobs: rep.blobs_received + rep.blobs_sent, ms: rep.duration_ms })}
                      </p>
                    )}
                  </Card>
                </li>
              );
            })}
          </ul>
        )}
      </section>

      {/* Pairing */}
      <section>
        <SectionTitle>{t("devices.pair")}</SectionTitle>
        <div className="grid gap-3 lg:grid-cols-2">
          <PairingCode />
          <EnterCode onPaired={() => void status.reload()} />
        </div>
        <details className="mt-3 text-sm">
          <summary className="cursor-pointer text-muted">{t("devices.addAddr")}</summary>
          <form
            className="mt-2 flex gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              if (addr.trim()) void api.call("sync.addAddr", { addr: addr.trim() }).then(() => setAddr(""));
            }}
          >
            <input
              value={addr}
              onChange={(e) => setAddr(e.target.value)}
              placeholder={t("devices.addrPlaceholder")}
              aria-label={t("devices.addAddr")}
              className="min-w-0 flex-1 rounded-full border border-line bg-surface px-4 py-2 font-mono text-sm outline-none focus:border-accent"
            />
            <Button type="submit">{t("app.add")}</Button>
          </form>
        </details>
      </section>
    </div>
  );
}
