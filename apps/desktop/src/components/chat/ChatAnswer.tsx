import { Fragment, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { ChatSource } from "../../gen/ChatSource";
import { cx } from "../ui";

/** Click handler for a source chip; sources without a target (a person, an unlinked event) are inert. */
export type OpenSource = (s: ChatSource) => void;

export function canOpen(s: ChatSource): boolean {
  return s.meeting_id != null;
}

/** `[n]` marker → numbered chip linking to the source (plain badge while it streams). */
function SourceChip({ n, source, onOpen }: { n: number; source?: ChatSource; onOpen: OpenSource }) {
  const { t } = useTranslation();
  const cls = "mx-0.5 inline-flex h-[18px] min-w-[18px] items-center justify-center rounded-md px-1 align-[1px] text-[11px] font-medium tabular-nums";
  if (!source) return <span className={cx(cls, "bg-sunken text-faint")}>{n}</span>;
  const label = t("chat.source", { n, label: source.label });
  return canOpen(source) ? (
    <button type="button" onClick={() => onOpen(source)} title={source.label} aria-label={label} className={cx(cls, "bg-accent-soft text-accent hover:bg-accent hover:text-accent-ink")}>
      {n}
    </button>
  ) : (
    <span title={source.label} aria-label={label} className={cx(cls, "bg-sunken text-muted")}>
      {n}
    </span>
  );
}

/** Inline Markdown subset: **bold**, `code`, and `[n]` citation chips. */
function inline(text: string, sources: ChatSource[], onOpen: OpenSource): ReactNode[] {
  const parts = text.split(/(\*\*[^*]+\*\*|`[^`]+`|\[\d+\])/g);
  return parts.map((p, i) => {
    if (p.startsWith("**") && p.endsWith("**") && p.length > 4) return <strong key={i}>{p.slice(2, -2)}</strong>;
    if (p.startsWith("`") && p.endsWith("`") && p.length > 2)
      return (
        <code key={i} className="rounded bg-sunken px-1 text-[0.9em]">
          {p.slice(1, -1)}
        </code>
      );
    const m = /^\[(\d+)\]$/.exec(p);
    if (m) {
      const n = Number(m[1]);
      return <SourceChip key={i} n={n} source={sources.find((s) => s.n === n)} onOpen={onOpen} />;
    }
    return <Fragment key={i}>{p}</Fragment>;
  });
}

type Block = { type: "p"; text: string } | { type: "h"; text: string } | { type: "ul"; items: string[] } | { type: "ol"; items: string[] };

/** Paragraphs, headings and (un)ordered lists: enough for the model's short answers. */
export function blocks(md: string): Block[] {
  const out: Block[] = [];
  let para: string[] = [];
  const flush = () => {
    if (para.length) out.push({ type: "p", text: para.join(" ") });
    para = [];
  };
  for (const raw of md.replace(/\r/g, "").split("\n")) {
    const line = raw.trim();
    const bullet = /^[-*•]\s+(.*)$/.exec(line);
    const num = /^\d+[.)]\s+(.*)$/.exec(line);
    const head = /^#{1,6}\s+(.*)$/.exec(line);
    if (!line) flush();
    else if (bullet || num) {
      flush();
      const type = bullet ? "ul" : "ol";
      const last = out[out.length - 1];
      const item = (bullet ?? num)![1];
      if (last && last.type === type) last.items.push(item);
      else out.push({ type, items: [item] });
    } else if (head) {
      flush();
      out.push({ type: "h", text: head[1] });
    } else para.push(line);
  }
  flush();
  return out;
}

export function ChatMarkdown({ text, sources, onOpen }: { text: string; sources: ChatSource[]; onOpen: OpenSource }) {
  return (
    <div className="flex flex-col gap-2 leading-relaxed">
      {blocks(text).map((b, i) => {
        if (b.type === "p") return <p key={i}>{inline(b.text, sources, onOpen)}</p>;
        if (b.type === "h")
          return (
            <p key={i} className="font-semibold">
              {inline(b.text, sources, onOpen)}
            </p>
          );
        const List = b.type === "ul" ? "ul" : "ol";
        return (
          <List key={i} className={cx("flex flex-col gap-1 pl-5", b.type === "ul" ? "list-disc" : "list-decimal")}>
            {b.items.map((it, k) => (
              <li key={k}>{inline(it, sources, onOpen)}</li>
            ))}
          </List>
        );
      })}
    </div>
  );
}

/** The sources an answer cited, as labelled chips under it. */
export function SourceList({ sources, onOpen }: { sources: ChatSource[]; onOpen: OpenSource }) {
  const { t } = useTranslation();
  if (!sources.length) return null;
  return (
    <div className="mt-2 flex flex-wrap items-center gap-1.5" aria-label={t("chat.sources")} role="list">
      {sources.map((s) => {
        const body = (
          <>
            <span className="tabular-nums text-faint">{s.n}</span>
            <span className="text-faint">{t(`chat.kind.${s.kind}`)}</span>
            <span className="max-w-[16rem] truncate">{s.label}</span>
          </>
        );
        const cls = "inline-flex max-w-full items-center gap-1.5 rounded-full border border-line bg-surface px-2 py-0.5 text-xs";
        return (
          <span role="listitem" key={s.n}>
            {canOpen(s) ? (
              <button type="button" onClick={() => onOpen(s)} className={cx(cls, "hover:border-accent hover:text-accent")} aria-label={t("chat.source", { n: s.n, label: s.label })}>
                {body}
              </button>
            ) : (
              <span className={cx(cls, "text-muted")}>{body}</span>
            )}
          </span>
        );
      })}
    </div>
  );
}
