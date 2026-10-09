import { describe, expect, it } from "vitest";
import { previewFleet } from "./preview";
import { activityCounts, machineDot, machineStatus, machineSummary, runnerStatus, tailnetState } from "./status";

const view = previewFleet();
const byName = Object.fromEntries(view.machines.map((m) => [m.displayName, m]));

describe("tailnetState", () => {
  it("says checking until the first result, and gives the reason on failure", () => {
    expect(tailnetState({ available: false, localOnline: null, tailnetName: null, asOf: null, error: null })).toEqual({
      kind: "checking",
      text: "Checking Tailscale…",
    });
    expect(tailnetState({ available: false, localOnline: false, tailnetName: null, asOf: null, error: "Tailscale is not connected (state: Stopped)" }).text).toBe(
      "Tailscale unavailable: Tailscale is not connected (state: Stopped)",
    );
    expect(tailnetState({ available: true, localOnline: true, tailnetName: "example.ts.net", asOf: null, error: null }).kind).toBe("ok");
  });
});

describe("status labels", () => {
  it("maps machine connections", () => {
    expect(machineStatus(byName["studio"].connection).label).toBe("Connected");
    expect(machineStatus(byName["build-linux-01"].connection).label).toBe("Stale");
    expect(machineStatus(byName["mac-mini-03"].connection).label).toBe("Error");
    expect(machineSummary(byName["studio"])).toBe("1/3 working");
    // A runner that isn't installed yet isn't "idle": the machine's state shows instead.
    expect(machineSummary(byName["mac-mini-03"])).toBe("Error");
    expect(machineSummary(byName["win-builder"])).toBe("Offline");
    const laptop = view.otherMachines.find((m) => m.displayName === "dev-laptop")!;
    expect(machineStatus(laptop.connection).label).toBe("Checking…");
  });

  it("maps runner activity", () => {
    const runners = view.machines.flatMap((m) => m.runners);
    expect(runnerStatus(runners.find((r) => r.name === "studio-1")!).label).toBe("Working");
    expect(runnerStatus(runners.find((r) => r.name === "win-builder")!).label).toBe("Offline");
  });

  it("shows a connected machine as busy while one of its runners works", () => {
    expect(machineDot(byName["studio"]).tone).toBe("busy");
    expect(machineDot(byName["build-linux-01"])).toEqual(machineStatus(byName["build-linux-01"].connection));
  });
});

describe("activityCounts", () => {
  it("counts by activity, with idle only for runners connected to GitHub", () => {
    expect(activityCounts(byName["studio"].runners)).toEqual({ busy: 1, idle: 1, offline: 0, unknown: 1 });
    const disconnected = structuredClone(byName["studio"].runners[1]);
    disconnected.connected = "no";
    expect(activityCounts([disconnected])).toEqual({ busy: 0, idle: 0, offline: 0, unknown: 0 });
    expect(activityCounts(byName["win-builder"].runners).offline).toBe(1);
  });

  it("agrees with the sidebar summary", () => {
    const m = structuredClone(byName["studio"]);
    m.runners = m.runners.filter((r) => r.activity.kind !== "busy");
    expect(machineSummary(m)).toBe(`${activityCounts(m.runners).idle} idle`);
  });
});
