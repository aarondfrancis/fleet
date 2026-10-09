import type { ReactNode } from "react";
import { useNow } from "../hooks";
import { elapsedSince, timeAgo } from "../lib/format";
import type { Tone } from "../lib/status";
import { errorText } from "../lib/tauri";
import { CopyValue } from "./CopyValue";
import { Icon } from "./icons";

export const toneVar: Record<Tone, string> = {
  busy: "var(--busy)",
  ok: "var(--ok)",
  warn: "var(--warn)",
  bad: "var(--bad)",
  muted: "var(--muted)",
  info: "var(--info)",
};

export function StatusDot({ tone, hollow, size = 8 }: { tone: Tone; hollow?: boolean; size?: number }) {
  const color = toneVar[tone];
  return (
    <span
      aria-hidden
      className="inline-block shrink-0 rounded-full"
      style={{
        width: size,
        height: size,
        background: hollow ? "transparent" : color,
        boxShadow: hollow ? `inset 0 0 0 1.5px ${color}` : undefined,
      }}
    />
  );
}

export function Pill({ tone, children, hollow }: { tone: Tone; children: ReactNode; hollow?: boolean }) {
  const color = toneVar[tone];
  return (
    <span
      className="inline-flex h-[20px] shrink-0 items-center gap-1.5 rounded-[6px] border px-1.5 text-[12px] font-medium whitespace-nowrap"
      style={{ color: tone === "ok" ? "var(--ok-ink)" : color, borderColor: `color-mix(in srgb, ${color} 25%, transparent)`, background: `color-mix(in srgb, ${color} 8%, transparent)` }}
    >
      <StatusDot tone={tone} hollow={hollow} size={6} />
      {children}
    </span>
  );
}

export function Label({ children }: { children: ReactNode }) {
  const className = "h-[20px] max-w-full items-center rounded-[4px] border border-line bg-sunken/60 px-1.5 font-mono text-[11px] text-ink-2 break-all";
  return typeof children === "string"
    ? <CopyValue value={children} label="Label" showIcon={false} className={className}>{children}</CopyValue>
    : <span className={`inline-flex ${className}`}>{children}</span>;
}

export function Card({ children, className = "" }: { children: ReactNode; className?: string }) {
  return <div className={`panel min-w-0 ${className}`}>{children}</div>;
}

export function Section({
  title,
  count,
  icon,
  action,
  children,
}: {
  title: string;
  count?: number;
  icon?: ReactNode;
  action?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="mt-8 min-w-0">
      <div className="mb-2 flex min-h-7 flex-wrap items-center gap-2">
        {icon && <span className="text-ink-3">{icon}</span>}
        <h2 className="text-[13px] font-semibold text-ink">{title}</h2>
        {count !== undefined && (
          <span className="font-mono text-[12px] font-normal text-ink-2 tabular">{count}</span>
        )}
        {action && <div className="ml-auto min-w-0">{action}</div>}
      </div>
      {children}
    </section>
  );
}

export function StatStrip({ children, cols = 4 }: { children: ReactNode; cols?: 3 | 4 }) {
  return <Card className={`grid gap-px overflow-hidden bg-line [&>*]:bg-card ${cols === 3 ? "grid-cols-3" : "grid-cols-2 lg:grid-cols-4"}`}>{children}</Card>;
}

export function Stat({
  label,
  aside,
  value,
  sub,
  stale,
  tone,
  hollow,
  children,
}: {
  label: string;
  /** Shown opposite the label, such as how old stale data is. */
  aside?: ReactNode;
  value: ReactNode;
  sub?: ReactNode;
  stale?: boolean;
  tone?: Tone;
  hollow?: boolean;
  children?: ReactNode;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-1.5 px-4 py-3">
      <div className="flex items-baseline justify-between gap-2">
        <span className="truncate text-[12px] font-medium text-ink-2">{label}</span>
        {aside}
      </div>
      <div className={`flex min-w-0 items-center gap-2 ${stale ? "opacity-45" : ""}`}>
        {tone && <StatusDot tone={tone} hollow={hollow} />}
        <span className="truncate text-[20px] font-semibold tracking-tight tabular" title={typeof value === "string" ? value : undefined}>{value}</span>
      </div>
      {children}
      {sub && <div className="truncate text-[12px] text-ink-2" title={typeof sub === "string" ? sub : undefined}>{sub}</div>}
    </div>
  );
}

/** Horizontal usage bar: amber from 70% used, red from 85%. Callers show no bar for missing data. */
export function Meter({ fraction, stale }: { fraction: number; stale?: boolean }) {
  const pct = Math.max(0, Math.min(1, fraction)) * 100;
  const color = pct >= 85 ? "var(--bad)" : pct >= 70 ? "var(--warn)" : "var(--meter)";
  return (
    <div className="h-1 w-full overflow-hidden rounded-full bg-sunken" style={{ opacity: stale ? 0.45 : 1 }}>
      <div className="h-full rounded-full" style={{ width: `${pct}%`, background: color }} />
    </div>
  );
}

export function Button({
  children,
  onClick,
  variant = "default",
  disabled,
  type = "button",
  small,
  "aria-label": ariaLabel,
}: {
  children: ReactNode;
  onClick?: () => void;
  variant?: "default" | "primary" | "danger" | "ghost";
  disabled?: boolean;
  type?: "button" | "submit";
  small?: boolean;
  "aria-label"?: string;
}) {
  const base = `inline-flex shrink-0 items-center justify-center gap-1.5 rounded-[6px] font-medium whitespace-nowrap transition-colors disabled:opacity-50 ${
    small ? "h-[28px] px-2.5 text-[12.5px]" : "h-[32px] px-3 text-[13px]"
  }`;
  const styles: Record<string, string> = {
    default: "border border-line bg-card text-ink hover:bg-hover active:bg-selected",
    primary: "bg-accent text-white hover:brightness-110 active:brightness-95",
    danger: "border border-bad/20 bg-bad/5 text-bad hover:bg-bad/10",
    ghost: "text-ink-2 hover:bg-hover hover:text-ink",
  };
  return (
    <button type={type} className={`${base} ${styles[variant]}`} onClick={onClick} disabled={disabled} aria-label={ariaLabel}>
      {children}
    </button>
  );
}

export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange: () => void; label: string; disabled?: boolean }) {
  return (
    <button
      type="button" role="switch" aria-checked={checked} aria-label={label} disabled={disabled} onClick={onChange}
      className={`relative h-6 w-10 shrink-0 rounded-full border transition-colors disabled:opacity-50 ${checked ? "border-accent bg-accent" : "border-line bg-sunken"}`}
    >
      <span className={`absolute top-0.5 left-0.5 h-[18px] w-[18px] rounded-full bg-white shadow-sm transition-transform ${checked ? "translate-x-4" : ""}`} />
    </button>
  );
}

/** A failed command's error, shown as is: Rust writes these as sentences. */
export function ErrorText({ error, className = "" }: { error: unknown; className?: string }) {
  return <p role="alert" className={`break-words text-[12px] text-bad selectable ${className}`}>{errorText(error)}</p>;
}

type CalloutTone = "bad" | "warn";

/** The tinted surface of a notice, for lists of notices built from rows. */
export const calloutSurface: Record<CalloutTone, string> = {
  bad: "border-bad/30 bg-bad/5",
  warn: "border-warn/30 bg-warn/5",
};

/** A notice on a page: something that went wrong or needs a look. */
export function Callout({ tone, title, children, className = "mt-4" }: { tone: CalloutTone; title?: ReactNode; children?: ReactNode; className?: string }) {
  return (
    <Card className={`flex items-start gap-2.5 px-4 py-3 text-[12.5px] ${calloutSurface[tone]} ${className}`}>
      <span className={`mt-px shrink-0 ${tone === "bad" ? "text-bad" : "text-warn"}`}><Icon name="alert" /></span>
      <div className="min-w-0 flex-1">
        {title && <div className="font-medium">{title}</div>}
        {children && <div className={`break-words text-ink-2 selectable ${title ? "mt-0.5" : ""}`}>{children}</div>}
      </div>
    </Card>
  );
}

export function EmptyLine({ children }: { children: ReactNode }) {
  return <div className="px-3.5 py-3 text-[12.5px] text-ink-3">{children}</div>;
}

/** A steady timer that ticks by itself, so its parent doesn't re-render every second. */
export function Elapsed({ since }: { since: string }) {
  const now = useNow();
  return <>{elapsedSince(since, now)}</>;
}

/** A relative age that ticks by itself, so its parent doesn't re-render every second. */
export function TimeAgo({ iso }: { iso: string | null }) {
  const now = useNow();
  return <>{timeAgo(iso, now)}</>;
}

export function KeyValue({ rows }: { rows: [string, ReactNode][] }) {
  return (
    <dl className="grid grid-cols-[minmax(110px,max-content)_1fr] gap-x-4 gap-y-1.5 text-[12.5px]">
      {rows.map(([k, v]) => (
        <div key={k} className="contents">
          <dt className="text-ink-2">{k}</dt>
          <dd className="min-w-0 truncate text-ink selectable">{v}</dd>
        </div>
      ))}
    </dl>
  );
}

export type KeyValueRow = [label: string, value: ReactNode, copyValue?: string];

export function KeyValuePanel({ title, rows, footer }: { title: string; rows: KeyValueRow[]; footer?: ReactNode }) {
  return (
    <Card className="overflow-hidden">
      <div className="border-b border-line bg-sunken/40 px-4 py-2 text-[12px] font-medium text-ink-2">{title}</div>
      <dl className="divide-y divide-line">
        {rows.map(([label, value, copyValue]) => (
          <div key={label} className="grid grid-cols-[minmax(70px,35%)_1fr] items-baseline gap-3 px-4 py-2 text-[13px]">
            <dt className="text-ink-2">{label}</dt>
            <dd className="min-w-0 truncate text-ink selectable" title={copyValue ?? (typeof value === "string" ? value : undefined)}>
              {copyValue ? <CopyValue value={copyValue} label={label} truncate>{value}</CopyValue> : value}
            </dd>
          </div>
        ))}
      </dl>
      {footer && <div className="border-t border-line px-4 py-2 text-[12px] text-ink-2">{footer}</div>}
    </Card>
  );
}
