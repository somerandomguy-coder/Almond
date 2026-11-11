// A small Markdown renderer for recipe answers: headings, bullet / numbered lists (nested by
// indent), tables, block quotes, rules, paragraphs; inline bold, italic, code and `[n]`
// citation markers, which become citation chips (`citations[n - 1]`). No raw HTML is rendered.
import type { ReactNode } from "react";
import type { SpanRef } from "../../gen/SpanRef";
import { CitationChip } from "../citations";

type Block =
  | { t: "h"; level: number; text: string }
  | { t: "p"; text: string }
  | { t: "list"; ordered: boolean; items: { indent: number; text: string; n?: number }[] }
  | { t: "table"; head: string[]; rows: string[][] }
  | { t: "quote"; text: string }
  | { t: "hr" };

const LIST_RE = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;

function splitRow(line: string): string[] {
  let s = line.trim();
  if (s.startsWith("|")) s = s.slice(1);
  if (s.endsWith("|")) s = s.slice(0, -1);
  return s.split("|").map((c) => c.trim());
}

const isSep = (line: string) => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(line);

export function parseBlocks(md: string): Block[] {
  const lines = md.replace(/\r\n/g, "\n").split("\n");
  const out: Block[] = [];
  let para: string[] = [];
  const flush = () => {
    if (para.length) out.push({ t: "p", text: para.join(" ") });
    para = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const trimmed = line.trim();
    if (!trimmed) {
      flush();
      continue;
    }
    const h = /^(#{1,6})\s+(.*)$/.exec(trimmed);
    if (h) {
      flush();
      out.push({ t: "h", level: h[1].length, text: h[2].replace(/\s*#+\s*$/, "") });
      continue;
    }
    if (/^(-{3,}|\*{3,}|_{3,})$/.test(trimmed)) {
      flush();
      out.push({ t: "hr" });
      continue;
    }
    if (trimmed.startsWith("|") && i + 1 < lines.length && isSep(lines[i + 1])) {
      flush();
      const head = splitRow(trimmed);
      const rows: string[][] = [];
      i += 2;
      while (i < lines.length && lines[i].trim().startsWith("|")) rows.push(splitRow(lines[i++]));
      i--;
      out.push({ t: "table", head, rows });
      continue;
    }
    const li = LIST_RE.exec(line);
    if (li) {
      flush();
      const ordered = /\d/.test(li[2]);
      const item = { indent: li[1].replace(/\t/g, "  ").length, text: li[3], n: ordered ? parseInt(li[2], 10) : undefined };
      const prev = out[out.length - 1];
      if (prev?.t === "list" && (prev.ordered === ordered || item.indent > 0)) prev.items.push(item);
      else out.push({ t: "list", ordered, items: [item] });
      continue;
    }
    if (trimmed.startsWith(">")) {
      flush();
      const prev = out[out.length - 1];
      const text = trimmed.replace(/^>\s?/, "");
      if (prev?.t === "quote") prev.text += " " + text;
      else out.push({ t: "quote", text });
      continue;
    }
    // A continuation line of a list item.
    const prev = out[out.length - 1];
    if (!para.length && prev?.t === "list" && /^\s+/.test(line)) {
      prev.items[prev.items.length - 1].text += " " + trimmed;
      continue;
    }
    para.push(trimmed);
  }
  flush();
  return out;
}

const INLINE_RE = /(\*\*[^*]+\*\*|__[^_]+__|`[^`]+`|\[\d+\](?!\()|\[[^\]]+\]\([^)]*\)|\*[^*\s][^*]*\*|(?<![\w])_[^_\s][^_]*_(?![\w]))/g;

function Inline({ text, citations }: { text: string; citations: SpanRef[] }): ReactNode {
  const parts = text.split(INLINE_RE);
  return parts.map((p, i) => {
    if (!p) return null;
    if (i % 2 === 0) return <span key={i}>{p}</span>;
    if (p.startsWith("**") || p.startsWith("__"))
      return (
        <strong key={i} className="font-semibold">
          <Inline text={p.slice(2, -2)} citations={citations} />
        </strong>
      );
    if (p.startsWith("`"))
      return (
        <code key={i} className="rounded bg-sunken px-1 text-[0.9em]">
          {p.slice(1, -1)}
        </code>
      );
    const cite = /^\[(\d+)\]$/.exec(p);
    if (cite) {
      const span = citations[Number(cite[1]) - 1];
      return span ? <CitationChip key={i} span={span} /> : null;
    }
    const link = /^\[([^\]]+)\]\(([^)]*)\)$/.exec(p);
    if (link) return <span key={i} className="underline decoration-dotted">{link[1]}</span>;
    return (
      <em key={i}>
        <Inline text={p.slice(1, -1)} citations={citations} />
      </em>
    );
  });
}

function ListView({ block, citations }: { block: Extract<Block, { t: "list" }>; citations: SpanRef[] }) {
  const base = Math.min(...block.items.map((x) => x.indent));
  return (
    <ul className="flex flex-col gap-1.5">
      {block.items.map((it, i) => {
        const level = Math.min(3, Math.floor((it.indent - base) / 2));
        return (
          <li key={i} className="flex items-start gap-2" style={{ marginLeft: `${level * 1.25}rem` }}>
            {it.n != null ? (
              <span className="min-w-[1.25rem] shrink-0 text-right text-muted tabular-nums">{it.n}.</span>
            ) : (
              <span className="mt-2.5 h-1 w-1 shrink-0 rounded-full bg-faint" />
            )}
            <div className="min-w-0 flex-1">
              <Inline text={it.text} citations={citations} />
            </div>
          </li>
        );
      })}
    </ul>
  );
}

/** Renders a recipe answer. */
export function Markdown({ text, citations }: { text: string; citations: SpanRef[] }) {
  const blocks = parseBlocks(text);
  return (
    <div className="flex flex-col gap-3 leading-relaxed" data-testid="recipe-markdown">
      {blocks.map((b, i) => {
        switch (b.t) {
          case "h": {
            const cls = b.level <= 1 ? "text-lg font-semibold" : b.level === 2 ? "mt-2 text-base font-semibold" : "text-sm font-semibold";
            return (
              <p key={i} role="heading" aria-level={Math.min(6, b.level + 2)} className={cls}>
                <Inline text={b.text} citations={citations} />
              </p>
            );
          }
          case "p":
            return (
              <p key={i}>
                <Inline text={b.text} citations={citations} />
              </p>
            );
          case "quote":
            return (
              <blockquote key={i} className="border-l-2 border-line pl-3 text-muted">
                <Inline text={b.text} citations={citations} />
              </blockquote>
            );
          case "hr":
            return <hr key={i} className="border-line" />;
          case "list":
            return <ListView key={i} block={b} citations={citations} />;
          case "table":
            return (
              <div key={i} className="overflow-x-auto rounded-xl border border-line">
                <table className="w-full border-collapse text-sm">
                  <thead className="bg-sunken text-left">
                    <tr>
                      {b.head.map((c, k) => (
                        <th key={k} className="px-3 py-2 font-medium">
                          <Inline text={c} citations={citations} />
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {b.rows.map((r, ri) => (
                      <tr key={ri} className="border-t border-line align-top">
                        {b.head.map((_, k) => (
                          <td key={k} className="px-3 py-2">
                            <Inline text={r[k] ?? ""} citations={citations} />
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            );
        }
      })}
    </div>
  );
}
