import { useCallback, useContext, useEffect, useRef, useState, type CSSProperties, type ReactNode, type RefObject } from "react";
import { useTranslation } from "react-i18next";
import type { Segment } from "../gen/Segment";
import type { SpanRef } from "../gen/SpanRef";
import type { Verdict } from "../gen/Verdict";
import type { VerdictLabel } from "../gen/VerdictLabel";
import { worstLabel } from "../lib/artifacts";
import { formatClock } from "../lib/text";
import { cx } from "./ui";

import { CitationContext, type CitationCtx } from "./citationContext";

const FALLBACK: CitationCtx = { resolve: () => undefined, speakerName: (s) => s ?? "", jump: () => {} };

/**
 * Fixed-position placement next to an anchor, clamped to the viewport, so floating panels are never
 * clipped by scrolling containers (summary column, transcript panel) and fit on a phone screen.
 */
function floatStyle(anchor: Element | null, width: number, prefer: "below" | "above"): CSSProperties {
  if (!anchor || typeof window === "undefined") return { display: "none" };
  const r = anchor.getBoundingClientRect();
  const vw = window.innerWidth;
  const vh = window.innerHeight;
  const w = Math.min(width, vw - 16);
  const left = Math.max(8, Math.min(r.left + r.width / 2 - w / 2, vw - w - 8));
  const below = vh - r.bottom - 8;
  const above = r.top - 8;
  const useBelow = prefer === "below" ? below >= 220 || below >= above : above < 120 && below > above;
  return useBelow
    ? { position: "fixed", left, width: w, top: r.bottom + 6, maxHeight: Math.max(160, below - 6), overflowY: "auto" }
    : { position: "fixed", left, width: w, bottom: vh - r.top + 6, maxHeight: Math.max(160, above - 6), overflowY: "auto" };
}

/** Close a floating panel when anything scrolls (its fixed position would go stale). */
function useCloseOnScroll(open: boolean, close: () => void, inside?: RefObject<HTMLElement | null>) {
  useEffect(() => {
    if (!open) return;
    const on = (e: Event) => {
      if (inside?.current && e.target instanceof Node && inside.current.contains(e.target)) return;
      close();
    };
    window.addEventListener("scroll", on, true);
    window.addEventListener("resize", on);
    return () => {
      window.removeEventListener("scroll", on, true);
      window.removeEventListener("resize", on);
    };
  }, [open, close, inside]);
}

function spanStart(span: SpanRef, seg?: Segment): number | null {
  return span.time_ms?.[0] ?? seg?.t0_ms ?? null;
}

/** Hover/focus shows the cited words; click jumps to the transcript segment + audio time. */
export function CitationChip({ span }: { span: SpanRef }) {
  const { t } = useTranslation();
  const { resolve, speakerName, jump } = useContext(CitationContext) ?? FALLBACK;
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  useCloseOnScroll(open, () => setOpen(false));
  const seg = resolve(span);
  const start = spanStart(span, seg);
  const label = start != null ? formatClock(start) : `#${span.segment + 1}`;
  const quote = seg ? (span.chars ? Array.from(seg.text).slice(span.chars[0], span.chars[1]).join("") : seg.text) : null;
  return (
    <span className="relative inline-block align-baseline" onMouseEnter={() => setOpen(true)} onMouseLeave={() => setOpen(false)}>
      <button
        ref={anchor}
        type="button"
        onClick={() => jump(span)}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        aria-label={`${t("meeting.jumpToTranscript")} ${label}`}
        className="ml-1 inline-flex h-5 items-center rounded-md bg-sunken px-1.5 text-[11px] font-medium tabular-nums text-muted hover:bg-accent-soft hover:text-accent"
      >
        {label}
      </button>
      {open && quote && (
        <span role="tooltip" style={floatStyle(anchor.current, 288, "above")} className="z-50 block rounded-xl border border-line bg-surface p-3 text-left text-sm font-normal text-ink shadow-lg shadow-black/10">
          <span className="mb-1 block text-xs text-muted">
            {speakerName(seg?.speaker ?? null)} · {t("meeting.sourceSegment", { time: label })}
          </span>
          “{quote}”
        </span>
      )}
    </span>
  );
}

export const VERDICT_STYLE: Record<VerdictLabel, { chip: string; underline: string; dot: string }> = {
  supported: { chip: "bg-ok-soft text-ok", underline: "", dot: "bg-ok" },
  contradicted: { chip: "bg-bad-soft text-bad", underline: "decoration-bad decoration-wavy underline underline-offset-4", dot: "bg-bad" },
  unverifiable: { chip: "bg-unk-soft text-unk", underline: "decoration-unk decoration-dotted underline underline-offset-4", dot: "bg-unk" },
  needs_review: { chip: "bg-warn-soft text-warn", underline: "decoration-warn decoration-dashed underline underline-offset-4", dot: "bg-warn" },
};

function Popover({ open, onClose, children, label, anchor }: { open: boolean; onClose: () => void; children: ReactNode; label: string; anchor: RefObject<HTMLElement | null> }) {
  const ref = useRef<HTMLDivElement>(null);
  useCloseOnScroll(open, onClose, ref);
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      const tgt = e.target as Node;
      if (ref.current && !ref.current.contains(tgt) && !anchor.current?.contains(tgt)) onClose();
    };
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, onClose, anchor]);
  if (!open) return null;
  return (
    <div ref={ref} role="dialog" aria-label={label} style={floatStyle(anchor.current, 340, "below")} className="z-50 rounded-xl border border-line bg-surface p-3 text-sm shadow-lg shadow-black/10">
      {children}
    </div>
  );
}

/** Badge with the most severe verdict; click opens the per-verifier evidence popover. */
export function VerdictBadge({ claims }: { claims: { claimText: string; verdicts: Verdict[] }[] }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLButtonElement>(null);
  const all = claims.flatMap((c) => c.verdicts);
  const label = worstLabel(all);
  const close = useCallback(() => setOpen(false), []);
  if (!label) return null;
  const st = VERDICT_STYLE[label];
  return (
    <span className="relative inline-block">
      <button
        ref={anchor}
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        aria-label={`${t("verdict.title")}: ${t(`verdict.${label}`)}`}
        className={cx("inline-flex h-5 items-center gap-1 rounded-full text-[11px] font-medium whitespace-nowrap", label === "supported" ? "px-1.5" : "px-2", st.chip)}
      >
        <span className={cx("h-1.5 w-1.5 rounded-full", st.dot)} />
        {/* Supported is the calm default: just a dot. Problems get a word. */}
        <span className={label === "supported" ? "sr-only" : undefined}>{t(`verdict.${label}`)}</span>
      </button>
      <Popover open={open} onClose={close} anchor={anchor} label={t("verdict.title")}>
        <p className="mb-2 text-xs font-semibold tracking-wider text-muted uppercase">{t("verdict.title")}</p>
        <ul className="flex flex-col gap-3">
          {claims.map((c, ci) => (
            <li key={ci} className="flex flex-col gap-2">
              {claims.length > 1 && <p className="text-xs text-muted">“{c.claimText}”</p>}
              {c.verdicts.map((v, i) => (
                <div key={i} className="rounded-lg bg-sunken p-2">
                  <div className="mb-1 flex flex-wrap items-center gap-2">
                    <span className={cx("rounded-full px-2 py-0.5 text-[11px] font-medium", VERDICT_STYLE[v.label].chip)}>{t(`verdict.${v.label}`)}</span>
                    <code className="text-xs text-muted">{v.verifier}</code>
                    <span className="ml-auto text-xs text-faint">{t("verdict.confidence", { pct: Math.round(v.confidence * 100) })}</span>
                  </div>
                  <p>{v.explanation}</p>
                  <div className="mt-1 text-xs text-muted">
                    {t("verdict.evidence")}: {v.evidence.length ? v.evidence.map((e, k) => <CitationChip key={k} span={e} />) : t("verdict.noEvidence")}
                  </div>
                  {v.trace != null && (
                    <details className="mt-1 text-xs text-muted">
                      <summary className="cursor-pointer">{t("verdict.trace")}</summary>
                      <pre className="mt-1 overflow-x-auto whitespace-pre-wrap">{JSON.stringify(v.trace, null, 2)}</pre>
                    </details>
                  )}
                </div>
              ))}
            </li>
          ))}
        </ul>
      </Popover>
    </span>
  );
}
