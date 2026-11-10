import { EditorContent, Extension, useEditor } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import { useState } from "react";
import type { NotesDoc } from "../lib/notesDoc";

function textToHtml(text: string): string {
  const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  return text
    .split("\n")
    .map((l) => `<p>${esc(l)}</p>`)
    .join("");
}

/** TipTap editor bound to the meeting's notes doc (Loro CRDT when available). */
export function NotesEditor({ doc, placeholder }: { doc: NotesDoc; placeholder: string }) {
  const [empty, setEmpty] = useState(!doc.text);
  const editor = useEditor(
    {
      extensions: [
        StarterKit.configure(doc.backend === "loro" ? { undoRedo: false } : {}),
        Extension.create({ name: "almondNotesBinding", addProseMirrorPlugins: () => doc.plugins() }),
      ],
      content: doc.backend === "memory" && doc.text ? textToHtml(doc.text) : undefined,
      editorProps: { attributes: { "aria-label": placeholder, role: "textbox", "aria-multiline": "true" } },
      onCreate: ({ editor }) => {
        doc.text = editor.getText({ blockSeparator: "\n" });
        setEmpty(editor.isEmpty);
      },
      onUpdate: ({ editor }) => {
        doc.text = editor.getText({ blockSeparator: "\n" });
        setEmpty(editor.isEmpty);
      },
    },
    [doc],
  );

  return (
    <div className="notes-editor relative" onClick={() => editor?.commands.focus()}>
      {empty && <p className="pointer-events-none absolute top-1 left-0 text-faint">{placeholder}</p>}
      <EditorContent editor={editor} />
    </div>
  );
}
