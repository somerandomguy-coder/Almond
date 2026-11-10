import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AlmondApi, ApiNotifications, Method, NotificationName, Params, Result, Unlisten } from "./types";

/** "meetings.list" → "meetings_list"; "notes.applyUpdate" → "notes_applyUpdate". */
export const commandName = (method: string) => method.replace(/\./g, "_");

/**
 * Real backend. Every API method is a Tauri command named like the method with '.' → '_',
 * invoked with a single `params` argument. Notifications are Tauri events with the same
 * '.' → '_' mapping (Tauri event names may not contain '.').
 */
export class TauriApi implements AlmondApi {
  readonly kind = "tauri" as const;
  private platformCache: Promise<string> | null = null;

  platform(): Promise<string> {
    if (!this.platformCache) {
      this.platformCache = invoke<{ platform: string }>("app_info")
        .then((i) => i.platform)
        .catch(() => "unknown");
    }
    return this.platformCache;
  }

  call<M extends Method>(method: M, params: Params<M>): Promise<Result<M>> {
    return invoke<Result<M>>(commandName(method), { params });
  }

  on<N extends NotificationName>(name: N, cb: (payload: ApiNotifications[N]) => void): Unlisten {
    let un: Unlisten | null = null;
    let cancelled = false;
    listen<ApiNotifications[N]>(commandName(name), (e) => cb(e.payload)).then((u) => {
      if (cancelled) u();
      else un = u;
    });
    return () => {
      cancelled = true;
      un?.();
    };
  }
}
