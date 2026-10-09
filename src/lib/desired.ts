import type { ChangeRequest } from "../bindings/ChangeRequest";
import type { DesiredRunnerRole } from "../bindings/DesiredRunnerRole";
import type { FleetPlanAction } from "../bindings/FleetPlanAction";
import type { FleetPlanItem } from "../bindings/FleetPlanItem";
import type { FleetStatusRow } from "../bindings/FleetStatusRow";
import type { FleetView } from "../bindings/FleetView";
import type { MachineView } from "../bindings/MachineView";
import type { RunnerView } from "../bindings/RunnerView";
import { activityCounts, type Tone } from "./status";

const ACTION_LABELS: Record<FleetPlanAction, string> = {
  install: "Install",
  "reconcile-labels": "Fix labels",
  "assign-group": "Move to trusted group",
  "start-service": "Start service",
  "retire-legacy": "Ready to retire",
  decommission: "Decommission",
  "no-change": "No change",
  restart: "Restart",
  inspect: "Can't check",
  "inspect-offline": "Offline on GitHub",
  "inspect-group": "Runner group not found",
  "preserve-legacy": "Keeping legacy",
  "preserve-service": "Busy, not restarting",
  "preserve-decommission": "Busy, not removing",
  "repair-registration": "Needs manual repair",
  "recover-local": "Needs manual recovery",
  "harden-group": "Runner group unsafe",
};

export function actionLabel(action: FleetPlanAction): string {
  return ACTION_LABELS[action];
}

export function itemTone(item: FleetPlanItem): Tone {
  if (item.blocked) return "warn";
  if (item.action === "no-change") return "ok";
  if (item.action === "decommission" || item.action === "retire-legacy") return "info";
  return "busy";
}

/** Plan items worth showing: anything but "no change". */
export function changes(items: FleetPlanItem[]): FleetPlanItem[] {
  return items.filter((i) => i.action !== "no-change");
}

export function roleLabel(role: DesiredRunnerRole): string {
  switch (role.kind) {
    case "declared":
      return "In the inventory";
    case "legacy":
      return `Legacy runner, replaced by ${role.replacedBy}`;
    case "decommission":
      return "Declared for decommission";
    case "undeclared":
      return "Not in the inventory";
  }
}

/** The runner's drift from the last check, unless it's in sync. */
export function driftText(r: RunnerView): string | null {
  const drift = r.desired?.status?.drift;
  return drift && drift !== "in sync" ? drift : null;
}

/** The inventory and this runner disagree, or it can't be checked. */
export function runnerNeedsChanges(r: RunnerView): boolean {
  return (r.desired?.plan ?? []).some((i) => i.action !== "no-change");
}

/** The machine has changes to apply or couldn't be read. */
export function machineNeedsChanges(m: MachineView): boolean {
  return (m.desired?.pendingChanges ?? 0) > 0 || !!m.desired?.error;
}

/** A change is running; other changes wait for it. */
export function operationRunning(view: FleetView): boolean {
  return view.plan.operation?.status === "running";
}

/** GitHub's own labels: not managed by the inventory. */
const BUILTIN_LABELS = new Set(["self-hosted", "macos", "linux", "windows", "x64", "arm64", "arm"]);

export function isBuiltinLabel(label: string): boolean {
  return BUILTIN_LABELS.has(label.toLowerCase());
}

/** Declared labels against GitHub's, for a side-by-side view. */
export function labelDiff(r: RunnerView): { label: string; state: "ok" | "missing" | "extra" }[] {
  const desired = r.desired?.desiredLabels ?? [];
  const actual = r.github?.labels ?? [];
  const out: { label: string; state: "ok" | "missing" | "extra" }[] = desired.map((label) => ({
    label,
    state: actual.includes(label) ? "ok" : "missing",
  }));
  for (const label of actual) {
    if (!desired.includes(label) && !isBuiltinLabel(label)) out.push({ label, state: "extra" });
  }
  return out;
}

interface RowAction {
  label: string;
  request: ChangeRequest;
  danger?: boolean;
}

/** Changes offered for one runner, each opening a review first. */
export function runnerActions(r: RunnerView): RowAction[] {
  const d = r.desired;
  if (!d) return [];
  const out: RowAction[] = [];
  const items = d.plan;
  const apply = items.find((i) => !i.blocked && i.apply);
  if (d.role.kind === "declared" && apply) {
    out.push({ label: actionLabel(apply.action), request: { kind: "apply", host: d.hostId, runner: r.name } });
  }
  if (d.role.kind === "legacy" && items.some((i) => i.action === "retire-legacy" && !i.blocked)) {
    out.push({ label: "Retire", request: { kind: "retire", host: d.hostId, runner: d.role.replacedBy }, danger: true });
  }
  if (d.role.kind === "decommission" && items.some((i) => i.action === "decommission" && !i.blocked)) {
    out.push({ label: "Decommission", request: { kind: "decommission", host: d.hostId, runner: r.name }, danger: true });
  }
  // Idle both at the last check and now: a review re-checks, but don't offer it for a busy runner.
  const idle = d.status?.busy === false && r.activity.kind !== "busy";
  if (serviceConfigured(d.status) && d.role.kind !== "decommission" && d.role.kind !== "undeclared" && idle) {
    out.push({ label: "Restart", request: { kind: "restart", runner: r.name } });
  }
  return out;
}

/** The last check found the runner's service installed on its host. */
function serviceConfigured(status: FleetStatusRow | null): boolean {
  const service = status?.serviceState;
  return service != null && service !== "missing" && service !== "unknown";
}

/** Can logs be read for this runner (an inventory runner with a configured directory)? */
export function hasLogs(r: RunnerView): boolean {
  const d = r.desired;
  return !!d && d.role.kind !== "undeclared" && serviceConfigured(d.status);
}

export function requestTitle(r: ChangeRequest): string {
  switch (r.kind) {
    case "apply":
      return r.runner ? `Apply ${r.runner}` : `Apply ${r.host}`;
    case "retire":
      return "Retire the legacy runner";
    case "decommission":
      return `Decommission ${r.runner}`;
    case "restart":
      return `Restart ${r.runner}`;
  }
}

/** The change is about this machine: its inventory host, or for a restart one of its runners. */
export function requestTargetsMachine(r: ChangeRequest, m: MachineView): boolean {
  if (r.kind === "restart") return m.runners.some((x) => x.name === r.runner);
  return !!m.desired && r.host === m.desired.host.id;
}

/** The change is about this runner, including a host-wide apply and retiring it as a legacy runner. */
export function requestTargetsRunner(r: ChangeRequest, runner: RunnerView): boolean {
  if (r.kind === "restart") return r.runner === runner.name;
  const d = runner.desired;
  if (!d || r.host !== d.hostId) return false;
  switch (r.kind) {
    case "apply":
      return r.runner == null || r.runner === runner.name;
    case "retire":
      // A retire names the replacement; the legacy runner is the one removed.
      return r.runner === runner.name || (d.role.kind === "legacy" && d.role.replacedBy === r.runner);
    case "decommission":
      return r.runner === runner.name;
  }
}

interface RunnerGroup {
  /** `depot-*` for a stem shared by several runners, else the runner's name. */
  name: string;
  runners: RunnerView[];
  online: number;
  busy: number;
}

/** Groups runners by name stem (`depot-x1`, `depot-x2` → `depot-*`), largest first. */
export function groupByStem(runners: RunnerView[]): RunnerGroup[] {
  const groups = new Map<string, RunnerView[]>();
  for (const r of runners) {
    const m = /^(.+?)[-_][^-_]+$/.exec(r.name);
    const stem = m ? `${m[1]}-*` : r.name;
    groups.set(stem, [...(groups.get(stem) ?? []), r]);
  }
  return [...groups.entries()]
    .map(([stem, rs]) => {
      const { busy, idle } = activityCounts(rs);
      return { name: rs.length === 1 ? rs[0].name : stem, runners: rs, online: busy + idle, busy };
    })
    .sort((a, b) => b.runners.length - a.runners.length || a.name.localeCompare(b.name));
}
