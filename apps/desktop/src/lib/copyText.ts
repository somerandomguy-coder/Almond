import type { ActionItem } from "../gen/ActionItem";
import type { EnhancedNotes } from "../gen/EnhancedNotes";
import type { Segment } from "../gen/Segment";
import type { Summary } from "../gen/Summary";
import { formatClock } from "./text";

/**
 * Plain-text / Markdown versions of a meeting's parts for the Copy buttons: no citation chips,
 * no verdict badges, speaker labels replaced by names.
 */

export interface TranscriptCopyOptions {
  /** "Lan: …" before each speaker turn. */
  speakers: boolean;
  /** "[12:34]" before each speaker turn. */
  timestamps: boolean;
}

/**
 * The transcript as readable paragraphs: consecutive segments by the same speaker are joined into
 * one paragraph; paragraphs are separated by a blank line.
 */
export function transcriptText(
  segments: Segment[],
  speakerName: (speaker: string | null) => string,
  opts: TranscriptCopyOptions = { speakers: true, timestamps: false },
): string {
  const paras: { speaker: string | null; t0: number; parts: string[] }[] = [];
  for (const s of [...segments].sort((a, b) => a.idx - b.idx)) {
    const text = s.text.trim();
    if (!text) continue;
    const last = paras.at(-1);
    if (last && last.speaker === s.speaker) last.parts.push(text);
    else paras.push({ speaker: s.speaker, t0: s.t0_ms, parts: [text] });
  }
  return paras
    .map((p) => {
      const prefix = [opts.timestamps ? `[${formatClock(p.t0)}]` : "", opts.speakers ? `${speakerName(p.speaker)}:` : ""].filter(Boolean).join(" ");
      const body = p.parts.join(" ");
      return prefix ? `${prefix} ${body}` : body;
    })
    .join("\n\n");
}

/** The summary as Markdown: `## Heading` and `- bullet`. */
export function summaryMarkdown(s: Summary, named: (t: string) => string = (t) => t): string {
  return s.sections
    .map((sec) => [sec.heading.trim() ? `## ${named(sec.heading.trim())}` : "", ...sec.bullets.map((b) => `- ${named(b.text.trim())}`)].filter(Boolean).join("\n"))
    .join("\n\n");
}

/** Enhanced notes as Markdown (headings, nested bullets, to-dos, paragraphs). */
export function enhancedMarkdown(n: EnhancedNotes, named: (t: string) => string = (t) => t): string {
  const out: string[] = [];
  for (const b of n.blocks) {
    const text = named(b.text.trim());
    if (!text) continue;
    const indent = "  ".repeat(Math.max(0, b.level));
    switch (b.style) {
      case "heading":
        if (out.length) out.push("");
        out.push(`## ${text}`);
        break;
      case "bullet":
        out.push(`${indent}- ${text}`);
        break;
      case "todo":
        out.push(`${indent}- [ ] ${text}`);
        break;
      default:
        if (out.length && out.at(-1) !== "") out.push("");
        out.push(text);
        out.push("");
    }
  }
  return out.join("\n").replace(/\n{3,}/g, "\n\n").trim();
}

/** Action items as a Markdown checklist: `- [ ] Text — Owner, due Friday`. */
export function actionItemsMarkdown(
  items: ActionItem[],
  named: (t: string) => string = (t) => t,
  labels: { due: string } = { due: "due" },
): string {
  return items
    .map((it) => {
      const meta = [it.owner ? named(it.owner) : "", it.due ? `${labels.due} ${it.due}` : ""].filter(Boolean).join(", ");
      return `- [ ] ${named(it.text.trim())}${meta ? ` — ${meta}` : ""}`;
    })
    .join("\n");
}

/** Everything for one meeting in one paste; empty parts are left out. */
export function meetingMarkdown(parts: { title: string; when: string; sections: { heading: string; text: string }[] }): string {
  const head = `# ${parts.title.trim() || "—"}\n${parts.when}`;
  const body = parts.sections
    .filter((s) => s.text.trim())
    // Demote the parts' own "## " headings under each section heading.
    .map((s) => `## ${s.heading}\n\n${s.text.trim().replace(/^## /gm, "### ")}`);
  return [head, ...body].join("\n\n") + "\n";
}

/** Write to the clipboard; falls back to a hidden textarea where the async API is unavailable. */
export async function copyToClipboard(text: string): Promise<void> {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(text);
      return;
    }
  } catch {
    // fall through
  }
  const ta = document.createElement("textarea");
  ta.value = text;
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  ta.select();
  const ok = document.execCommand?.("copy");
  ta.remove();
  if (!ok) throw new Error("clipboard unavailable");
}
