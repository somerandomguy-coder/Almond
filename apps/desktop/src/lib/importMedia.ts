// Picking / dropping an audio or video file for `meetings.import`.
import { useEffect, useRef, useState } from "react";
import type { AlmondApi } from "../api/types";

/** Extensions offered by the file dialog (the core decodes more through ffmpeg when installed). */
export const MEDIA_EXTENSIONS = [
  "wav", "mp3", "m4a", "aac", "flac", "ogg", "oga", "opus", "wma", "aiff", "aif", "caf",
  "mp4", "m4v", "mov", "mkv", "webm", "avi", "3gp",
];

const isMedia = (path: string) => MEDIA_EXTENSIONS.includes(path.split(".").pop()?.toLowerCase() ?? "");

export const baseName = (path: string) => path.split(/[\\/]/).pop() ?? path;

/** The mock API stands in for the native dialog (`?mock=1`, tests). */
interface PicksFiles {
  pickFile(): Promise<string | null>;
}
const picksFiles = (api: AlmondApi): api is AlmondApi & PicksFiles => typeof (api as Partial<PicksFiles>).pickFile === "function";

/** Native "open file" dialog; `null` when closed without choosing. */
export async function pickMediaFile(api: AlmondApi, opts: { title: string; filterName: string }): Promise<string | null> {
  if (api.kind === "tauri") {
    const { open } = await import("@tauri-apps/plugin-dialog");
    const r = await open({
      title: opts.title,
      multiple: false,
      directory: false,
      filters: [{ name: opts.filterName, extensions: MEDIA_EXTENSIONS }],
    });
    return typeof r === "string" ? r : null;
  }
  return picksFiles(api) ? api.pickFile() : null;
}

/**
 * Files dropped onto the window. In the Tauri app the webview reports real paths
 * (`onDragDropEvent`); in the browser (mock) the HTML5 drop only has a file name, which the
 * mock API accepts as a path. Returns whether a drag is over the window.
 */
export function useFileDrop(api: AlmondApi, onDrop: (path: string) => void, enabled = true): boolean {
  const [over, setOver] = useState(false);
  const cb = useRef(onDrop);
  useEffect(() => {
    cb.current = onDrop;
  });
  useEffect(() => {
    if (!enabled) return;
    let un: (() => void) | null = null;
    let cancelled = false;
    if (api.kind === "tauri") {
      void import("@tauri-apps/api/webview")
        .then(({ getCurrentWebview }) =>
          getCurrentWebview().onDragDropEvent((e) => {
            const p = e.payload;
            if (p.type === "enter" || p.type === "over") setOver(true);
            else if (p.type === "leave") setOver(false);
            else if (p.type === "drop") {
              setOver(false);
              const path = p.paths.find(isMedia) ?? p.paths[0];
              if (path) cb.current(path);
            }
          }),
        )
        .then((u) => {
          if (cancelled) u();
          else un = u;
        })
        .catch(() => {});
      return () => {
        cancelled = true;
        un?.();
      };
    }
    const hasFiles = (e: DragEvent) => Array.from(e.dataTransfer?.types ?? []).includes("Files");
    const onOver = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      setOver(true);
    };
    const onLeave = (e: DragEvent) => {
      if (e.relatedTarget == null) setOver(false);
    };
    const onDropEv = (e: DragEvent) => {
      if (!hasFiles(e)) return;
      e.preventDefault();
      setOver(false);
      const f = e.dataTransfer?.files?.[0];
      if (f) cb.current(f.name);
    };
    window.addEventListener("dragover", onOver);
    window.addEventListener("dragleave", onLeave);
    window.addEventListener("drop", onDropEv);
    return () => {
      window.removeEventListener("dragover", onOver);
      window.removeEventListener("dragleave", onLeave);
      window.removeEventListener("drop", onDropEv);
    };
  }, [api, enabled]);
  return over;
}
