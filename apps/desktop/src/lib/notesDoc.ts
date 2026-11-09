// The user's own meeting notes live in a Loro CRDT document (Meeting.notes_doc), replicated as
// base64 Loro updates via `notes.get` / `notes.applyUpdate`. The editor binds to it through
// loro-prosemirror. `NotesDoc` is the seam: if Loro (WASM) can't load, a plain in-memory doc is
// used so the UI keeps working (notes then aren't persisted — TODO surfaced in the UI as a warning).
import { keymap } from "@tiptap/pm/keymap";
import type { Plugin } from "@tiptap/pm/state";
import type { AlmondApi } from "../api/types";
import { b64ToBytes, bytesToB64 } from "./text";

export interface NotesDoc {
  readonly docId: string;
  /** "loro" when CRDT-backed and persisted through the API; "memory" for the fallback. */
  readonly backend: "loro" | "memory";
  /** ProseMirror plugins that bind an editor to this doc (empty for the memory backend). */
  plugins(): Plugin[];
  /** Plain text of the notes, used as input for "Enhance notes". Updated by the editor. */
  text: string;
  destroy(): void;
}

class MemoryNotesDoc implements NotesDoc {
  readonly backend = "memory" as const;
  text = "";
  constructor(readonly docId: string) {}
  plugins(): Plugin[] {
    return [];
  }
  destroy() {}
}

/** Remember text per doc for the memory backend so switching tabs doesn't lose notes. */
const memoryText = new Map<string, string>();

export async function openNotesDoc(api: AlmondApi, docId: string, opts: { useLoro?: boolean } = {}): Promise<NotesDoc> {
  if (opts.useLoro !== false) {
    try {
      const [{ LoroDoc }, lpm] = await Promise.all([import("loro-crdt"), import("loro-prosemirror")]);
      const doc = new LoroDoc();
      const snap = await api.call("notes.get", { doc: docId });
      for (const u of snap.updates_b64) doc.import(b64ToBytes(u));
      const unsub = doc.subscribeLocalUpdates((bytes: Uint8Array) => {
        void api.call("notes.applyUpdate", { doc: docId, update_b64: bytesToB64(bytes) }).catch(() => {});
      });
      // loro-prosemirror's LoroDocType is a LoroDoc with a typed schema; ours is untyped.
      const loroDoc = doc as unknown as Parameters<typeof lpm.LoroSyncPlugin>[0]["doc"];
      const nd: NotesDoc = {
        docId,
        backend: "loro",
        text: "",
        plugins: () => [
          lpm.LoroSyncPlugin({ doc: loroDoc }),
          lpm.LoroUndoPlugin({ doc: loroDoc }),
          keymap({ "Mod-z": lpm.undo, "Mod-y": lpm.redo, "Mod-Shift-z": lpm.redo }),
        ],
        destroy: () => unsub(),
      };
      return nd;
    } catch (e) {
      console.warn("Loro unavailable, notes fall back to memory:", e);
    }
  }
  const m = new MemoryNotesDoc(docId);
  m.text = memoryText.get(docId) ?? "";
  return new Proxy(m, {
    set(target, prop, value) {
      if (prop === "text") memoryText.set(docId, value as string);
      return Reflect.set(target, prop, value);
    },
  });
}
