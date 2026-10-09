import { describe, expect, it } from "vitest";
import type { FleetMachineEvent } from "../bindings/FleetMachineEvent";
import { previewFleet } from "./preview";
import { mergeEvents, monitoringMachine } from "./monitoring";

describe("monitoring history", () => {
  it("merges overlapping cursor pages without numeric precision loss", () => {
    const event = (id: string, detail = id) => ({ id, detail } as FleetMachineEvent);
    const first = [event("18446744073709551613"), event("18446744073709551612", "current")];
    const older = [event("18446744073709551612", "duplicate"), event("18446744073709551611")];
    expect(mergeEvents(first, older).map((e) => e.detail)).toEqual(["18446744073709551613", "current", "18446744073709551611"]);
  });

  it("resolves stable inventory and discovered-machine keys to their current selection ID", () => {
    const view = previewFleet();
    const machine = view.machines[0];
    machine.id = "ts:replacement-node";
    machine.monitoring.machineKey = "inv:unchanged-host";
    expect(monitoringMachine(view, "inv:unchanged-host")?.id).toBe("ts:replacement-node");
    const other = view.otherMachines[0];
    expect(monitoringMachine(view, other.monitoring.machineKey)).toBe(other);
    expect(monitoringMachine(view, "inv:removed")).toBeUndefined();
  });
});
