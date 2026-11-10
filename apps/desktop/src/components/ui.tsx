import type { ButtonHTMLAttributes, ReactNode, SVGProps } from "react";
import { useTranslation } from "react-i18next";
import { errorMessage } from "../api/errors";

export function cx(...c: (string | false | null | undefined)[]) {
  return c.filter(Boolean).join(" ");
}

type Variant = "primary" | "secondary" | "ghost" | "danger";

export function Button({
  variant = "secondary",
  size = "md",
  className,
  ...rest
}: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: Variant; size?: "sm" | "md" | "lg" }) {
  return (
    <button
      type="button"
      {...rest}
      className={cx(
        "inline-flex items-center justify-center gap-2 rounded-full font-medium transition-colors focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent disabled:cursor-not-allowed disabled:opacity-50",
        size === "sm" && "h-8 px-3 text-sm",
        size === "md" && "h-10 px-4 text-sm",
        size === "lg" && "h-14 px-8 text-base",
        variant === "primary" && "bg-accent text-accent-ink hover:brightness-95",
        variant === "secondary" && "border border-line bg-surface text-ink hover:bg-sunken",
        variant === "ghost" && "text-muted hover:bg-sunken hover:text-ink",
        variant === "danger" && "border border-line bg-surface text-bad hover:bg-bad-soft",
        className,
      )}
    />
  );
}

export function Card({ className, children }: { className?: string; children: ReactNode }) {
  return <div className={cx("rounded-2xl border border-line bg-surface p-4 sm:p-5", className)}>{children}</div>;
}

export function SectionTitle({ children, right }: { children: ReactNode; right?: ReactNode }) {
  return (
    <div className="mb-3 flex items-center justify-between gap-2">
      <h2 className="text-xs font-semibold uppercase tracking-wider text-muted">{children}</h2>
      {right}
    </div>
  );
}

export function Pill({ className, children, title }: { className?: string; children: ReactNode; title?: string }) {
  return (
    <span title={title} className={cx("inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium", className ?? "bg-sunken text-muted")}>
      {children}
    </span>
  );
}

export function Progress({ value, className, label }: { value: number; className?: string; label?: string }) {
  const pct = Math.round(Math.max(0, Math.min(1, value)) * 100);
  return (
    <div role="progressbar" aria-label={label} aria-valuenow={pct} aria-valuemin={0} aria-valuemax={100} className={cx("h-1.5 w-full overflow-hidden rounded-full bg-sunken", className)}>
      <div className="h-full rounded-full bg-accent transition-[width] duration-300" style={{ width: `${pct}%` }} />
    </div>
  );
}

export function Toggle({ checked, onChange, label, id }: { checked: boolean; onChange: (v: boolean) => void; label: string; id?: string }) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
      className={cx("relative h-6 w-11 shrink-0 rounded-full transition-colors", checked ? "bg-accent" : "bg-line")}
    >
      <span className={cx("absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform", checked ? "translate-x-5" : "translate-x-0.5")} />
    </button>
  );
}

export function Spinner({ className }: { className?: string }) {
  return <span aria-hidden className={cx("inline-block h-4 w-4 animate-spin rounded-full border-2 border-current border-t-transparent", className)} />;
}

export function Loading() {
  const { t } = useTranslation();
  return (
    <div className="flex items-center gap-2 p-6 text-sm text-muted">
      <Spinner /> {t("app.loading")}
    </div>
  );
}

export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  const { t } = useTranslation();
  return (
    <div role="alert" className="flex flex-wrap items-center gap-3 rounded-xl bg-bad-soft p-3 text-sm text-bad">
      {t("app.error", { message: errorMessage(error) })}
      {onRetry && (
        <Button size="sm" onClick={onRetry}>
          {t("app.retry")}
        </Button>
      )}
    </div>
  );
}

/** «match» → <mark>match</mark> */
export function Highlighted({ text }: { text: string }) {
  const parts = text.split(/(«[^»]*»)/g);
  return (
    <>
      {parts.map((p, i) =>
        p.startsWith("«") ? (
          <mark key={i} className="rounded bg-accent-soft px-0.5 text-ink">
            {p.slice(1, -1)}
          </mark>
        ) : (
          <span key={i}>{p}</span>
        ),
      )}
    </>
  );
}

// ---- icons (inline, 1.5px stroke, currentColor) ----
const base = (p: SVGProps<SVGSVGElement>) => ({ width: 18, height: 18, viewBox: "0 0 24 24", fill: "none", stroke: "currentColor", strokeWidth: 1.6, strokeLinecap: "round" as const, strokeLinejoin: "round" as const, "aria-hidden": true, ...p });
export const Icon = {
  home: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="M3 10.5 12 3l9 7.5V20a1 1 0 0 1-1 1h-5v-6h-6v6H4a1 1 0 0 1-1-1z" />
    </svg>
  ),
  sync: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="M20 12a8 8 0 0 1-14.3 4.9M4 12a8 8 0 0 1 14.3-4.9" />
      <path d="M18.5 3v4.2h-4.2M5.5 21v-4.2h4.2" />
    </svg>
  ),
  settings: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
    </svg>
  ),
  search: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <circle cx="11" cy="11" r="7" />
      <path d="m20 20-3.5-3.5" />
    </svg>
  ),
  mic: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <rect x="9" y="3" width="6" height="12" rx="3" />
      <path d="M5 11a7 7 0 0 0 14 0M12 18v3" />
    </svg>
  ),
  trash: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3" />
    </svg>
  ),
  stop: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <rect x="6" y="6" width="12" height="12" rx="2" fill="currentColor" />
    </svg>
  ),
  sparkle: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="M12 3v4M12 17v4M3 12h4M17 12h4M6 6l2.5 2.5M15.5 15.5 18 18M6 18l2.5-2.5M15.5 8.5 18 6" />
    </svg>
  ),
  calendar: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <rect x="3" y="5" width="18" height="16" rx="2" />
      <path d="M3 10h18M8 3v4M16 3v4" />
    </svg>
  ),
  video: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <rect x="3" y="6" width="13" height="12" rx="2" />
      <path d="m16 10 5-3v10l-5-3z" />
    </svg>
  ),
  people: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <circle cx="9" cy="8" r="3.5" />
      <path d="M2.5 20a6.5 6.5 0 0 1 13 0M16 4.5a3.5 3.5 0 0 1 0 7M21.5 20a6.5 6.5 0 0 0-4-6" />
    </svg>
  ),
  plus: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="M12 5v14M5 12h14" />
    </svg>
  ),
  back: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="M15 5l-7 7 7 7" />
    </svg>
  ),
  laptop: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <rect x="4" y="5" width="16" height="11" rx="1.5" />
      <path d="M2 19h20" />
    </svg>
  ),
  phone: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <rect x="7" y="2.5" width="10" height="19" rx="2" />
      <path d="M11 18.5h2" />
    </svg>
  ),
  desktop: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <rect x="3" y="4" width="18" height="12" rx="1.5" />
      <path d="M8 20h8M12 16v4" />
    </svg>
  ),
  play: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="M7 5v14l11-7z" fill="currentColor" />
    </svg>
  ),
  check: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="m5 12.5 4.5 4.5L19 7" />
    </svg>
  ),
  bell: (p: SVGProps<SVGSVGElement>) => (
    <svg {...base(p)}>
      <path d="M6 16V11a6 6 0 1 1 12 0v5l1.5 2h-15zM10 20.5a2 2 0 0 0 4 0" />
    </svg>
  ),
};

export function PlatformIcon({ platform, ...p }: { platform: string } & SVGProps<SVGSVGElement>) {
  if (platform === "android") return <Icon.phone {...p} />;
  if (platform === "linux") return <Icon.laptop {...p} />;
  return <Icon.desktop {...p} />;
}
