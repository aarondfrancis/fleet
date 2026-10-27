import type { FleetView } from "../bindings/FleetView";
import type { MachineView } from "../bindings/MachineView";
import type { RunnerView } from "../bindings/RunnerView";

/** What the detail pane shows. Keys are stable across refreshes. */
export type Selection =
  | { kind: "overview" }
  | { kind: "queue" }
  | { kind: "activity" }
  | { kind: "machine"; id: string }
  | { kind: "runner"; key: string }
  | { kind: "ignored" };

export function selectionId(s: Selection): string {
  switch (s.kind) {
    case "overview":
    case "queue":
    case "activity":
    case "ignored":
      return s.kind;
    case "machine":
      return `machine:${s.id}`;
    case "runner":
      return `runner:${s.key}`;
  }
}

interface NavItem {
  selection: Selection;
  /** Machine id for runner rows (keyboard ← goes to the parent). */
  parent?: string;
}

/** Which collapsible sidebar groups are open. */
export interface OpenGroups {
  /** Runners GitHub lists that aren't on any machine. */
  unlinked: boolean;
  /** Tailnet machines without runners. */
  other: boolean;
}

/** Every machine: runner machines, then other tailnet machines. */
export function allMachines(view: FleetView): MachineView[] {
  return [...view.machines, ...view.otherMachines];
}

export function findMachine(view: FleetView, id: string | null): MachineView | undefined {
  return id ? allMachines(view).find((m) => m.id === id) : undefined;
}

/** Every runner: those on machines, then those not linked to one. */
export function allRunners(view: FleetView): RunnerView[] {
  return [...view.machines.flatMap((m) => m.runners), ...view.unmatchedRunners];
}

export function findRunner(view: FleetView, key: string): RunnerView | undefined {
  return allRunners(view).find((r) => r.key === key);
}

/** Filter only the navigation tree; the detail pane keeps the complete live view. */
export function filterNavView(view: FleetView, query: string): FleetView {
  const q = query.trim().toLowerCase();
  if (!q) return view;
  const matches = (values: (string | null | undefined)[]) => values.some((v) => v?.toLowerCase().includes(q));
  const machineMatches = (m: MachineView) => matches([m.displayName, m.description, m.host, m.desired?.host.id, m.tailnet?.hostname, m.tailnet?.dnsName]);
  const runnerMatches = (r: RunnerView) => matches([r.name, r.org, ...r.github?.labels ?? [], ...r.desired?.desiredLabels ?? [], r.currentJob?.repo, r.currentJob?.name]);
  return {
    ...view,
    machines: view.machines.flatMap((m) => {
      if (machineMatches(m)) return [m];
      const runners = m.runners.filter(runnerMatches);
      return runners.length ? [{ ...m, runners }] : [];
    }),
    otherMachines: view.otherMachines.filter(machineMatches),
    unmatchedRunners: view.unmatchedRunners.filter(runnerMatches),
    ignored: view.ignored.filter((m) => matches([m.peer.hostname, m.peer.dnsName])),
  };
}

/** Sidebar rows in visual order, honoring collapsed machines and groups. */
export function flattenNav(view: FleetView, collapsed: ReadonlySet<string>, open: OpenGroups): NavItem[] {
  const items: NavItem[] = [{ selection: { kind: "overview" } }, { selection: { kind: "queue" } }, { selection: { kind: "activity" } }];
  for (const m of view.machines) {
    items.push({ selection: { kind: "machine", id: m.id } });
    if (!collapsed.has(m.id)) {
      for (const r of m.runners) items.push({ selection: { kind: "runner", key: r.key }, parent: m.id });
    }
  }
  if (open.unlinked) {
    for (const r of view.unmatchedRunners) items.push({ selection: { kind: "runner", key: r.key } });
  }
  if (open.other) {
    for (const m of view.otherMachines) items.push({ selection: { kind: "machine", id: m.id } });
  }
  if (view.ignored.length > 0) items.push({ selection: { kind: "ignored" } });
  return items;
}

export function moveSelection(items: NavItem[], current: Selection, delta: 1 | -1): Selection {
  if (items.length === 0) return current;
  const id = selectionId(current);
  const idx = items.findIndex((i) => selectionId(i.selection) === id);
  if (idx === -1) return items[0].selection;
  const next = Math.min(items.length - 1, Math.max(0, idx + delta));
  return items[next].selection;
}

/** If the selected thing disappeared (a removed or ignored machine), fall back sensibly. */
export function reconcileSelection(view: FleetView, s: Selection): Selection {
  switch (s.kind) {
    case "machine":
      return findMachine(view, s.id) ? s : { kind: "overview" };
    case "runner":
      return findRunner(view, s.key) ? s : { kind: "overview" };
    case "ignored":
      return view.ignored.length > 0 ? s : { kind: "overview" };
    default:
      return s;
  }
}
