import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { copyToClipboard } from "../lib/copyText";
import { Button, cx, Icon } from "./ui";

export interface CopyOption {
  label: string;
  text: () => string;
}

/**
 * "Copy" button: puts `text()` on the clipboard and shows "Copied" for a moment. With `options`,
 * a small menu next to it offers other formats (e.g. transcript with timestamps).
 */
export function CopyButton({
  text,
  label,
  options,
  className,
  testId,
}: {
  text: () => string;
  /** Accessible name and visible label; default "Copy". */
  label?: string;
  options?: CopyOption[];
  className?: string;
  testId?: string;
}) {
  const { t } = useTranslation();
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");
  const [open, setOpen] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const menu = useRef<HTMLDivElement>(null);
  useEffect(() => () => clearTimeout(timer.current), []);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!menu.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", close);
    return () => document.removeEventListener("mousedown", close);
  }, [open]);

  const copy = async (get: () => string) => {
    setOpen(false);
    clearTimeout(timer.current);
    try {
      await copyToClipboard(get());
      setState("copied");
    } catch {
      setState("failed");
    }
    timer.current = setTimeout(() => setState("idle"), 1800);
  };

  const name = label ?? t("copy.copy");
  return (
    <div ref={menu} className={cx("relative inline-flex items-center", className)} data-testid={testId}>
      <Button size="sm" variant="ghost" onClick={() => void copy(text)} aria-label={name} title={name} className="whitespace-nowrap">
        {state === "copied" ? <Icon.check width={14} height={14} /> : <Icon.copy width={14} height={14} />}
        <span aria-live="polite">{state === "copied" ? t("copy.copied") : state === "failed" ? t("copy.failed") : name}</span>
      </Button>
      {options && options.length > 0 && (
        <>
          <Button size="sm" variant="ghost" aria-label={t("copy.more")} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen((o) => !o)} className="px-1.5">
            <span aria-hidden>▾</span>
          </Button>
          {open && (
            <div role="menu" className="absolute top-full right-0 z-20 mt-1 min-w-48 rounded-xl border border-line bg-surface p-1 text-sm shadow-lg">
              {options.map((o) => (
                <button key={o.label} type="button" role="menuitem" onClick={() => void copy(o.text)} className="block w-full rounded-lg px-3 py-1.5 text-left hover:bg-sunken">
                  {o.label}
                </button>
              ))}
            </div>
          )}
        </>
      )}
    </div>
  );
}
