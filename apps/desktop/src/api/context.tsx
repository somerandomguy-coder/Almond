import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { MockApi } from "./mock/MockApi";
import { TauriApi } from "./tauri";
import type { AlmondApi, ApiNotifications, NotificationName } from "./types";

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/** Mock when not inside Tauri, or when `?mock=1`. `?platform=android` makes the mock pretend. */
export function createApi(search: string = typeof window !== "undefined" ? window.location.search : ""): AlmondApi {
  const q = new URLSearchParams(search);
  if (q.get("mock") === "1" || !isTauri()) {
    const api = new MockApi({
      platform: q.get("platform") ?? "windows",
      speed: Number(q.get("speed") ?? 1) || 1,
      reminderAfterMs: q.get("reminder") === "0" ? null : 8000,
    });
    if (typeof window !== "undefined") (window as unknown as { __almondMock: MockApi }).__almondMock = api;
    return api;
  }
  return new TauriApi();
}

const ApiContext = createContext<AlmondApi | null>(null);

export function ApiProvider({ api, children }: { api: AlmondApi; children: ReactNode }) {
  return <ApiContext.Provider value={api}>{children}</ApiContext.Provider>;
}

export function useApi(): AlmondApi {
  const api = useContext(ApiContext);
  if (!api) throw new Error("useApi outside ApiProvider");
  return api;
}

/** Subscribe to a server notification for the lifetime of the component. */
export function useNotification<N extends NotificationName>(name: N, cb: (payload: ApiNotifications[N]) => void) {
  const api = useApi();
  const ref = useRef(cb);
  ref.current = cb;
  useEffect(() => api.on(name, (p) => ref.current(p)), [api, name]);
}

/** Minimal async loader: { data, error, loading, reload }. */
export function useLoad<T>(fn: () => Promise<T>, deps: unknown[]) {
  const [data, setData] = useState<T | undefined>(undefined);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(true);
  const seq = useRef(0);
  // eslint-disable-next-line react-hooks/exhaustive-deps
  const run = useCallback(fn, deps);
  const reload = useCallback(() => {
    const my = ++seq.current;
    setLoading(true);
    return run()
      .then((d) => {
        if (my === seq.current) {
          setData(d);
          setError(null);
        }
      })
      .catch((e) => my === seq.current && setError(e))
      .finally(() => my === seq.current && setLoading(false));
  }, [run]);
  useEffect(() => {
    void reload();
  }, [reload]);
  return { data, error, loading, reload, setData };
}
