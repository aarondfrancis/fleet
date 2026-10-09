import { useMutation } from "@tanstack/react-query";
import type { FleetView } from "../../bindings/FleetView";
import type { HealthView } from "../../bindings/HealthView";
import type { JobView } from "../../bindings/JobView";
import type { RunnerView } from "../../bindings/RunnerView";
import { memoryUsedFraction, primaryDisk, diskUsedFraction } from "../../lib/health";
import { useNow } from "../../hooks";
import { formatBytes, formatClock, formatPercent, timeAgo } from "../../lib/format";
import { driftText, isBuiltinLabel } from "../../lib/desired";
import { busySince, runnerAsOf, runnerStatus } from "../../lib/status";
import { commands } from "../../lib/tauri";
import { findMachine } from "../../lib/nav";
import { useUi } from "../../store";
import { Card, Elapsed, ErrorText, Meter, Stat, StatStrip, StatusDot, toneVar } from "../ui";
import { Icon } from "../icons";
import { CopyValue } from "../CopyValue";

export function PageHeader({
  title,
  titleText,
  subtitle,
  status,
  actions,
  icon,
}: {
  title: React.ReactNode;
  titleText?: string;
  subtitle?: React.ReactNode;
  status?: React.ReactNode;
  actions?: React.ReactNode;
  icon?: React.ReactNode;
}) {
  return (
    <header className="flex flex-wrap items-start gap-3">
      {icon && (
        <div className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-line bg-card text-ink-2 shadow-xs">
          {icon}
        </div>
      )}
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <h1 aria-label={titleText} className="break-words text-[20px] font-semibold tracking-tight selectable">{title}</h1>
          {status}
        </div>
        {subtitle && <div className="mt-1 break-words text-[13px] text-ink-2">{subtitle}</div>}
      </div>
      {actions && <div className="ml-auto flex max-w-full flex-wrap items-center gap-2">{actions}</div>}
    </header>
  );
}

/** One runner as a list row: status, name, what it's doing, steady timer. */
export function RunnerLine({ r, view, showMachine }: { r: RunnerView; view: FleetView; showMachine?: boolean }) {
  const select = useUi((s) => s.select);
  const now = useNow();
  const status = runnerStatus(r);
  const since = busySince(r);
  const machine = showMachine ? findMachine(view, r.machineId)?.displayName : null;
  const drift = driftText(r);
  const asOf = runnerAsOf(r);
  const detail = r.currentJob
    ? `${r.currentJob.repo} · ${r.name}`
    : r.activity.kind === "busy"
      ? "Job details unavailable"
      : drift ?? (r.github?.labels.filter((l) => !isBuiltinLabel(l)).join(", ") || null);
  return (
    <button type="button"
      className="flex min-h-[56px] w-full items-center gap-3 border-b border-line px-4 py-2.5 text-left last:border-b-0 hover:bg-hover"
      onClick={() => select({ kind: "runner", key: r.key })}
    >
      <StatusDot tone={status.tone} hollow={status.hollow} />
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="truncate font-medium">{r.currentJob?.workflowName ?? r.currentJob?.name ?? r.name}</span>
          {machine && <span className="truncate text-[11.5px] text-ink-3">{machine}</span>}
        </div>
        {detail && <div className={`truncate font-mono text-[12px] ${detail === drift ? "text-warn" : "text-ink-2"}`}>{detail}</div>}
      </div>
      <span className="shrink-0 text-[12px] text-ink-2 tabular">
        {since ? <Elapsed since={since} /> : status.label}
        {asOf && <span className="ml-1 text-ink-3">· {timeAgo(asOf, now)}</span>}
      </span>
      <span className="text-ink-3">
        <Icon name="chevron" size={11} />
      </span>
    </button>
  );
}

export function List({ children }: { children: React.ReactNode }) {
  return <Card className="overflow-hidden">{children}</Card>;
}

export function RunnerCells({ runners }: { runners: RunnerView[] }) {
  return (
    <span
      className="flex flex-wrap items-center gap-0.5"
      role="img"
      aria-label={runners.map((r) => `${r.name}: ${runnerStatus(r).label}`).join(", ")}
    >
      {runners.map((r) => {
        const status = runnerStatus(r);
        const color = toneVar[status.tone];
        const outlined = status.hollow || r.activity.kind === "offline";
        return (
          <span
            key={r.key}
            title={`${r.name}: ${status.label}${status.hollow ? " (last known)" : ""}`}
            className="h-3.5 w-2 rounded-[2px]"
            style={{ background: outlined ? undefined : color, border: outlined ? `1px dashed ${color}` : undefined }}
          />
        );
      })}
    </span>
  );
}

export function HealthMeters({ h }: { h: HealthView }) {
  const now = useNow();
  const memUsed = memoryUsedFraction(h);
  const disk = primaryDisk(h);
  const asOf = h.stale && <span className="shrink-0 text-[11px] text-warn">as of {timeAgo(h.asOf, now)}</span>;
  return (
    <StatStrip cols={3}>
      <Stat
        label="CPU"
        aside={asOf}
        stale={h.stale}
        value={h.cpuPercent == null ? "—" : formatPercent(h.cpuPercent)}
        sub={h.loadAverage ? `load ${h.loadAverage.map((l) => l.toFixed(1)).join(" ")}` : h.logicalCores == null ? "unavailable" : `${h.logicalCores} cores`}
      >
        <MeterSlot fraction={h.cpuPercent == null ? null : h.cpuPercent / 100} stale={h.stale} />
      </Stat>
      <Stat
        label="Memory"
        aside={asOf}
        stale={h.stale}
        value={memUsed == null ? "—" : formatPercent(memUsed * 100)}
        sub={`${formatBytes(h.memoryAvailableBytes)} free of ${formatBytes(h.memoryTotalBytes)}`}
      >
        <MeterSlot fraction={memUsed} stale={h.stale} />
      </Stat>
      <Stat
        label="Disk free"
        aside={asOf}
        stale={h.stale}
        value={disk ? formatBytes(disk.availableBytes) : "—"}
        sub={disk ? `of ${formatBytes(disk.totalBytes)}` : "not reported"}
      >
        <MeterSlot fraction={diskUsedFraction(disk)} stale={h.stale} />
      </Stat>
    </StatStrip>
  );
}

/** Keeps the cards aligned when one has no reading. */
function MeterSlot({ fraction, stale }: { fraction: number | null; stale: boolean }) {
  return fraction == null ? <div className="h-1" /> : <Meter fraction={fraction} stale={stale} />;
}

/** Opens a GitHub page in the browser, showing why if it can't. */
export function GitHubLink({ url, className, label, children }: { url: string; className: string; label?: string; children: React.ReactNode }) {
  const open = useMutation({ mutationFn: () => commands.openGithubUrl(url) });
  return (
    <>
      <button className={className} aria-label={label} onClick={() => open.mutate()}>{children}</button>
      {open.isError && <ErrorText error={open.error} className="w-full" />}
    </>
  );
}

export function JobCard({ job, since }: { job: JobView; since: string | null }) {
  return (
    <Card className="border-busy/25 bg-busy/5 px-4 py-3">
      <div className="flex items-start gap-3">
        <span className="mt-0.5 text-busy"><Icon name="runner" size={16} /></span>
        <div className="min-w-0 flex-1">
          <div className="truncate text-[13px] font-medium selectable">{job.name}</div>
          <div className="mt-0.5 truncate font-mono text-[12px] text-ink-2 selectable">
            {job.workflowName && <>{job.workflowName} · </>}<CopyValue value={job.repo} label="Repository" />{job.branch && <> · <CopyValue value={job.branch} label="Branch" /></>}
          </div>
        </div>
        <div className="text-right">
          <div className="text-[20px] font-semibold tabular">{since ? <Elapsed since={since} /> : "—"}</div>
          <div className="text-[11px] text-ink-3">{since ? `since ${formatClock(since)}` : ""}</div>
        </div>
      </div>
      {job.htmlUrl && (
        <div className="mt-2.5 flex flex-wrap items-center gap-2">
          <GitHubLink url={job.htmlUrl} className="inline-flex items-center gap-1 text-[12px] text-accent hover:underline">
            Open on GitHub <Icon name="external" size={11} />
          </GitHubLink>
        </div>
      )}
    </Card>
  );
}
