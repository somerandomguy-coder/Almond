import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import type { RecordingStatus } from "../gen/RecordingStatus";
import { useApi, useNotification } from "../api/context";

export type Theme = "system" | "light" | "dark";

interface AppState {
  platform: string;
  recording: RecordingStatus;
  theme: Theme;
  setTheme: (t: Theme) => void;
}

const Ctx = createContext<AppState | null>(null);

const THEME_KEY = "almond.theme";

function readTheme(): Theme {
  try {
    const t = localStorage.getItem(THEME_KEY);
    if (t === "light" || t === "dark" || t === "system") return t;
  } catch {
    /* ignore */
  }
  return "system";
}

function applyTheme(t: Theme) {
  if (typeof document === "undefined") return;
  const dark = t === "dark" || (t === "system" && typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", dark);
}

export function AppStateProvider({ children }: { children: ReactNode }) {
  const api = useApi();
  const [platform, setPlatform] = useState("");
  const [recording, setRecording] = useState<RecordingStatus>({ state: "idle", meeting_id: null, started_at: null, elapsed_ms: 0, levels: [] });
  const [theme, setThemeState] = useState<Theme>(readTheme);

  useEffect(() => {
    void api.platform().then(setPlatform);
    void api.call("recording.status", {}).then(setRecording).catch(() => {});
  }, [api]);
  useNotification("recording.status", setRecording);

  useEffect(() => {
    applyTheme(theme);
    if (theme !== "system" || typeof window.matchMedia !== "function") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const on = () => applyTheme("system");
    mq.addEventListener?.("change", on);
    return () => mq.removeEventListener?.("change", on);
  }, [theme]);

  const setTheme = useCallback(
    (t: Theme) => {
      setThemeState(t);
      try {
        localStorage.setItem(THEME_KEY, t);
      } catch {
        /* ignore */
      }
      void api.call("settings.set", { key: "ui.theme", value: t }).catch(() => {});
    },
    [api],
  );

  return <Ctx.Provider value={{ platform, recording, theme, setTheme }}>{children}</Ctx.Provider>;
}

export function useAppState(): AppState {
  const s = useContext(Ctx);
  if (!s) throw new Error("useAppState outside AppStateProvider");
  return s;
}
