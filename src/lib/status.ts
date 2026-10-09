import type { MachineConnection } from "../bindings/MachineConnection";
import type { MachineView } from "../bindings/MachineView";
import type { RunnerView } from "../bindings/RunnerView";
import type { TailnetStatusView } from "../bindings/TailnetStatusView";

/** Visual tone; each maps to one calm color. */
export type Tone = "busy" | "ok" | "warn" | "bad" | "muted" | "info";

interface StatusLabel {
  tone: Tone;
  label: string;
  /** Hollow dot: the state is inferred from old data or unknown. */
  hollow?: boolean;
}

export function runnerStatus(r: RunnerView): StatusLabel {
  // Declared in the inventory, and the last check found it isn't there yet.
  if (r.installed === "no" && r.activity.kind === "unknown") return { tone: "muted", label: "Not installed", hollow: true };
  switch (r.activity.kind) {
    case "busy":
      return { tone: "busy", label: "Working" };
    case "idle":
      return r.connected === "yes"
        ? { tone: "ok", label: "Idle" }
        : { tone: "ok", label: "Not working", hollow: true };
    case "offline":
      return { tone: "bad", label: "Offline" };
    case "unknown": {
      // Only older GitHub data (e.g. from the last check): show it, hollow.
      const g = r.github;
      if (g?.status === "online") return g.busy ? { tone: "busy", label: "Working", hollow: true } : { tone: "ok", label: "Idle", hollow: true };
      if (g) return { tone: "bad", label: "Offline", hollow: true };
      return { tone: "muted", label: "Unknown", hollow: true };
    }
  }
}

/** When the facts behind a runner's state are older data, how old. */
export function runnerAsOf(r: RunnerView): string | null {
  return r.activity.kind === "unknown" && r.github?.stale ? r.github.asOf : null;
}

export function machineStatus(c: MachineConnection): StatusLabel {
  switch (c.kind) {
    case "connected":
      return { tone: "ok", label: "Connected" };
    case "stale":
      return { tone: "warn", label: "Stale", hollow: true };
    case "offline":
      return { tone: "muted", label: "Offline" };
    case "error":
      return { tone: "bad", label: "Error" };
    case "pending":
      return { tone: "muted", label: "Checking…", hollow: true };
    case "disabled":
      return { tone: "muted", label: "Disabled", hollow: true };
  }
}

/** Runners by what they're doing now. Idle counts only runners connected to GitHub. */
export interface ActivityCounts {
  busy: number;
  idle: number;
  offline: number;
  unknown: number;
}

export function activityCounts(runners: RunnerView[]): ActivityCounts {
  const counts: ActivityCounts = { busy: 0, idle: 0, offline: 0, unknown: 0 };
  for (const r of runners) {
    if (r.activity.kind === "idle") {
      if (r.connected === "yes") counts.idle++;
    } else counts[r.activity.kind]++;
  }
  return counts;
}

/** Short right-aligned sidebar summary for a machine row: its runners when
 * it has any, otherwise its own state. */
export function machineSummary(m: MachineView): string {
  const total = m.runners.length;
  if (m.connection.kind === "offline") return "Offline";
  if (total === 0) return machineStatus(m.connection).label;
  const { busy, idle } = activityCounts(m.runners);
  if (busy > 0) return `${busy}/${total} working`;
  if (idle > 0) return `${idle} idle`;
  // None online or known: the machine's own state says more than a count.
  return machineStatus(m.connection).label;
}

/** The machine's dot: its connection, or busy while a connected machine's runner works. */
export function machineDot(m: MachineView): StatusLabel {
  const status = machineStatus(m.connection);
  const working = m.connection.kind === "connected" && m.runners.some((r) => r.activity.kind === "busy");
  return working ? { ...status, tone: "busy" } : status;
}

/** Hover text for a machine: its name and personal description. */
export function machineTooltip(m: MachineView): string {
  return [m.displayName, m.description].filter(Boolean).join("\n");
}

/** Tailscale state for display: "checking" until the first result arrives. */
export function tailnetState(t: TailnetStatusView): { kind: "ok" | "checking" | "error"; text: string } {
  if (t.available) return { kind: "ok", text: t.tailnetName ?? "Tailscale" };
  if (t.error) return { kind: "error", text: `Tailscale unavailable: ${t.error}` };
  return { kind: "checking", text: "Checking Tailscale…" };
}

export function hasWarning(r: RunnerView): boolean {
  return r.warnings.length > 0;
}

/** When a busy runner's work started, if known. */
export function busySince(r: RunnerView): string | null {
  if (r.activity.kind !== "busy") return null;
  return r.activity.since ?? r.currentJob?.startedAt ?? null;
}

/** Fresh preferences, or advertised host keys when preferences are unavailable. */
export function hasTailscaleSsh(m: MachineView): boolean {
  const check = !m.diagnostics?.stale && m.diagnostics?.checks.find((c) => c.id === "tailscale-ssh");
  if (check && check.status !== "unknown") return check.value === "Enabled";
  return !!m.tailnet?.sshHostKeys.length;
}
