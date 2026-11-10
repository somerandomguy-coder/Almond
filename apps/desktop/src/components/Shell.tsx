import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { Link, NavLink, Outlet, useLocation } from "react-router";
import { ErrorBoundary } from "./ErrorBoundary";
import { useApi } from "../api/context";
import { formatClock } from "../lib/text";
import { useAppState } from "../state/AppState";
import { ReminderToast } from "./ReminderToast";
import { cx, Icon } from "./ui";

const NAV = [
  { to: "/", key: "nav.home", icon: Icon.home, end: true },
  { to: "/devices", key: "nav.devices", icon: Icon.sync, end: false },
  { to: "/settings", key: "nav.settings", icon: Icon.settings, end: false },
] as const;

/** Runs `sync.now` periodically while the app is open, if the user enabled it. */
function useAutoSync() {
  const api = useApi();
  useEffect(() => {
    let timer: ReturnType<typeof setInterval> | null = null;
    let alive = true;
    const tick = async () => {
      try {
        const on = await api.call("settings.get", { key: "sync.auto_while_open" });
        if (on === true) {
          const st = await api.call("sync.status", {});
          if (!st.syncing) await api.call("sync.now", {});
        }
      } catch {
        /* best effort */
      }
    };
    timer = setInterval(() => alive && void tick(), 120_000);
    return () => {
      alive = false;
      if (timer) clearInterval(timer);
    };
  }, [api]);
}

export function Shell() {
  const { t } = useTranslation();
  const api = useApi();
  const { recording } = useAppState();
  const location = useLocation();
  useAutoSync();

  const recPill =
    recording.state !== "idle" && recording.meeting_id ? (
      <Link
        to={`/meeting/${recording.meeting_id}`}
        className="inline-flex items-center gap-2 rounded-full bg-rec/10 px-3 py-1 text-sm font-medium text-rec"
      >
        <span className="rec-dot h-2 w-2 rounded-full bg-rec" />
        {t("nav.recording", { time: formatClock(recording.elapsed_ms) })}
      </Link>
    ) : null;

  return (
    <div className="flex h-full flex-col md:flex-row">
      {/* Sidebar (tablet/desktop) */}
      <aside className="hidden w-60 shrink-0 flex-col border-r border-line bg-bg px-3 py-5 md:flex">
        <Link to="/" className="mb-6 flex items-center gap-2 px-3 text-lg font-semibold tracking-tight">
          <span aria-hidden className="inline-block h-5 w-3.5 rotate-12 rounded-[50%] bg-accent" />
          {t("app.name")}
        </Link>
        <nav className="flex flex-col gap-1">
          {NAV.map(({ to, key, icon: I, end }) => (
            <NavLink
              key={to}
              to={to}
              end={end}
              className={({ isActive }) =>
                cx("flex items-center gap-3 rounded-xl px-3 py-2 text-sm", isActive ? "bg-sunken font-medium text-ink" : "text-muted hover:bg-sunken hover:text-ink")
              }
            >
              <I />
              {t(key)}
            </NavLink>
          ))}
        </nav>
        <div className="mt-auto flex flex-col gap-3 px-3">
          {recPill}
          {api.kind === "mock" && (
            <span title={t("app.mockHint")} className="w-fit rounded-full border border-dashed border-line px-2 py-0.5 text-xs text-faint">
              {t("app.mockBadge")}
            </span>
          )}
        </div>
      </aside>

      {/* Top bar (phone) */}
      <header className="flex items-center justify-between border-b border-line px-4 py-3 md:hidden">
        <Link to="/" className="flex items-center gap-2 font-semibold">
          <span aria-hidden className="inline-block h-4 w-3 rotate-12 rounded-[50%] bg-accent" />
          {t("app.name")}
        </Link>
        <div className="flex items-center gap-2">
          {recPill}
          {api.kind === "mock" && !recPill && <span className="rounded-full border border-dashed border-line px-2 py-0.5 text-xs text-faint">{t("app.mockBadge")}</span>}
        </div>
      </header>

      <main className="min-h-0 flex-1 overflow-y-auto pb-20 md:pb-0">
        <ErrorBoundary resetKey={location.pathname}>
          <Outlet />
        </ErrorBoundary>
      </main>

      {/* Bottom tabs (phone) */}
      <nav className="fixed inset-x-0 bottom-0 z-40 flex border-t border-line bg-surface/95 backdrop-blur md:hidden" style={{ paddingBottom: "env(safe-area-inset-bottom)" }}>
        {NAV.map(({ to, key, icon: I, end }) => (
          <NavLink
            key={to}
            to={to}
            end={end}
            className={({ isActive }) => cx("flex flex-1 flex-col items-center gap-0.5 py-2 text-[11px]", isActive ? "text-accent" : "text-muted")}
          >
            <I />
            {t(key)}
          </NavLink>
        ))}
      </nav>

      <ReminderToast />
    </div>
  );
}
