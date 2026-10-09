import { describe, expect, it } from "vitest";
import { driftText, groupByStem, labelDiff, machineNeedsChanges, requestTargetsMachine, requestTargetsRunner, roleLabel, runnerActions } from "./desired";
import { previewFleet } from "./preview";

const view = previewFleet();
const machine = (name: string) => view.machines.find((m) => m.displayName === name)!;
const runner = (name: string) => view.machines.flatMap((m) => m.runners).find((r) => r.name === name)!;

describe("runnerActions", () => {
  it("offers retire on the legacy runner, naming its replacement", () => {
    expect(runnerActions(runner("Studio-Old-2"))).toEqual([
      { label: "Retire", request: { kind: "retire", host: "studio", runner: "studio-2" }, danger: true },
      { label: "Restart", request: { kind: "restart", runner: "Studio-Old-2" } },
    ]);
  });

  it("offers the apply step for drift and never restarts a busy runner", () => {
    expect(runnerActions(runner("mini-02"))).toEqual([
      { label: "Fix labels", request: { kind: "apply", host: "mac-mini-02", runner: "mini-02" } },
    ]);
    expect(runnerActions(runner("studio-1"))).toEqual([]);
  });

  it("offers decommission only for declared runners, and nothing for unreachable hosts", () => {
    const checked = structuredClone(runner("linux-01-b"));
    checked.desired!.plan = [{ ...checked.desired!.plan[0], action: "decommission", apply: false, blocked: false }];
    expect(runnerActions(checked).map((a) => a.label)).toEqual(["Decommission"]);
    checked.desired!.role = { kind: "undeclared" };
    expect(runnerActions(checked)).toEqual([]);
    expect(runnerActions(runner("linux-01-b"))).toEqual([]);
    expect(runnerActions(runner("win-builder"))).toEqual([]);
  });

  it("requires an unblocked apply classification supplied by the plan", () => {
    const checked = structuredClone(runner("mini-03-01"));
    const item = { ...checked.desired!.plan[0], action: "start-service" as const, apply: true, blocked: true };
    checked.desired!.plan = [item];
    expect(runnerActions(checked)).toEqual([]);
    item.blocked = false;
    expect(runnerActions(checked)).toEqual([
      { label: "Start service", request: { kind: "apply", host: "mac-mini-03", runner: "mini-03-01" } },
    ]);
    item.apply = false;
    expect(runnerActions(checked)).toEqual([]);
  });

  it("offers install for a declared runner that isn't there yet", () => {
    const checked = structuredClone(runner("mini-03-01"));
    checked.desired!.plan = [{ ...checked.desired!.plan[0], action: "install", apply: true, blocked: false }];
    expect(runnerActions(checked)).toEqual([
      { label: "Install", request: { kind: "apply", host: "mac-mini-03", runner: "mini-03-01" } },
    ]);
    // An SSH failure cannot establish that the runner is missing.
    expect(runnerActions(runner("mini-03-01"))).toEqual([]);
  });
});

describe("desired state on runners and machines", () => {
  it("describes roles and drift", () => {
    expect(roleLabel(runner("Studio-Old-2").desired!.role)).toBe("Legacy runner, replaced by studio-2");
    expect(driftText(runner("studio-2"))).toBeNull();
    expect(driftText(runner("mini-03-01"))).toBe("host unavailable");
    expect(driftText(runner("win-builder"))).toBe("host unavailable");
  });

  it("flags machines with changes or that couldn't be read", () => {
    expect(machineNeedsChanges(machine("mac-mini-02"))).toBe(true);
    expect(machineNeedsChanges(machine("win-builder"))).toBe(true); // couldn't be read
  });

  it("compares declared labels with GitHub's", () => {
    const states = Object.fromEntries(labelDiff(runner("mini-02")).map((l) => [l.label, l.state]));
    expect(states["macos-build"]).toBe("missing");
    expect(states["self-hosted"]).toBeUndefined(); // GitHub's own label, not managed
  });
});

describe("which pages show a change", () => {
  it("shows a restart on its runner and on the machine it runs on", () => {
    const restart = { kind: "restart", runner: "studio-1" } as const;
    expect(requestTargetsRunner(restart, runner("studio-1"))).toBe(true);
    expect(requestTargetsRunner(restart, runner("studio-2"))).toBe(false);
    expect(requestTargetsMachine(restart, machine("studio"))).toBe(true);
    expect(requestTargetsMachine(restart, machine("mac-mini-02"))).toBe(false);
  });

  it("matches host changes by inventory host, including a host-wide apply on each runner", () => {
    const apply = { kind: "apply", host: "studio", runner: null } as const;
    expect(requestTargetsMachine(apply, machine("studio"))).toBe(true);
    expect(requestTargetsMachine(apply, view.otherMachines[0])).toBe(false);
    expect(requestTargetsRunner(apply, runner("studio-2"))).toBe(true);
    expect(requestTargetsRunner({ ...apply, runner: "studio-1" }, runner("studio-2"))).toBe(false);
    expect(requestTargetsRunner({ kind: "decommission", host: "build-linux-01", runner: "linux-01-b" }, runner("linux-01-b"))).toBe(true);
  });

  it("shows a retire on the legacy runner it removes", () => {
    const retire = { kind: "retire", host: "studio", runner: "studio-2" } as const;
    expect(requestTargetsRunner(retire, runner("Studio-Old-2"))).toBe(true);
    expect(requestTargetsRunner(retire, runner("studio-1"))).toBe(false);
  });
});

describe("groupByStem", () => {
  it("groups by name stem with counts", () => {
    const runners = ["depot-a1", "depot-b2", "depot-c3"].map((name, i) => ({
      ...view.unmatchedRunners[0],
      key: `k${i}`,
      name,
    }));
    expect(groupByStem([...runners, view.unmatchedRunners[0]])).toMatchObject([
      { name: "depot-*", runners: { length: 3 } },
      { name: "cloud-gpu-1", runners: { length: 1 } },
    ]);
  });
});
