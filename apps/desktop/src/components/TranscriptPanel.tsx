import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import type { Segment } from "../gen/Segment";
import { formatClock } from "../lib/text";
import { cx } from "./ui";

export interface TranscriptLine {
  segment: Segment;
  final: boolean;
}

const SPEAKER_COLORS = ["text-accent", "text-[#6d5bd0] dark:text-[#b3a6ff]", "text-[#b0632a] dark:text-[#f0a46e]", "text-[#2a7ab0] dark:text-[#7ec0f0]"];

export function TranscriptPanel({
  lines,
  live,
  focus,
  speakerName,
  onSeek,
}: {
  lines: TranscriptLine[];
  live: boolean;
  /** Segment idx to scroll to + highlight; `nonce` re-triggers the flash on repeated clicks. */
  focus: { idx: number; nonce: number } | null;
  speakerName: (id: string | null) => string;
  onSeek: (ms: number) => void;
}) {
  const { t } = useTranslation();
  const scroller = useRef<HTMLDivElement>(null);
  const refs = useRef(new Map<number, HTMLLIElement>());
  const speakerIdx = new Map<string, number>();
  const colorFor = (s: string | null) => {
    if (!s) return "text-muted";
    if (!speakerIdx.has(s)) speakerIdx.set(s, speakerIdx.size);
    return SPEAKER_COLORS[speakerIdx.get(s)! % SPEAKER_COLORS.length];
  };

  // Follow the live stream unless the user scrolled up.
  useEffect(() => {
    const el = scroller.current;
    if (!live || !el) return;
    const nearBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    if (nearBottom) el.scrollTop = el.scrollHeight;
  }, [lines, live]);

  useEffect(() => {
    if (!focus) return;
    const el = refs.current.get(focus.idx);
    el?.scrollIntoView?.({ block: "center", behavior: "smooth" });
  }, [focus]);

  return (
    <div ref={scroller} className="h-full overflow-y-auto" data-testid="transcript">
      {lines.length === 0 ? (
        <p className="p-4 text-sm text-muted">{live ? t("meeting.transcriptEmpty") : t("meeting.transcriptNone")}</p>
      ) : (
        <ol className="flex flex-col gap-1 p-2">
          {lines.map(({ segment: s, final }) => {
            const focused = focus?.idx === s.idx;
            return (
              <li
                key={`${s.idx}-${focused ? focus?.nonce : 0}`}
                ref={(el) => {
                  if (el) refs.current.set(s.idx, el);
                  else refs.current.delete(s.idx);
                }}
                data-segment={s.idx}
                aria-current={focused ? "true" : undefined}
                className={cx("rounded-xl px-3 py-2", focused && "flash ring-1 ring-accent/40")}
              >
                <div className="mb-0.5 flex items-center gap-2 text-xs">
                  <span className={cx("font-medium", colorFor(s.speaker))}>{speakerName(s.speaker)}</span>
                  <button type="button" onClick={() => onSeek(s.t0_ms)} className="tabular-nums text-faint hover:text-accent">
                    {formatClock(s.t0_ms)}
                  </button>
                  {s.lang && <span className="rounded bg-sunken px-1 text-[10px] text-faint uppercase">{s.lang}</span>}
                </div>
                <p className={cx("text-[15px] leading-relaxed", !final && "text-faint italic")}>{s.text}</p>
              </li>
            );
          })}
        </ol>
      )}
    </div>
  );
}
