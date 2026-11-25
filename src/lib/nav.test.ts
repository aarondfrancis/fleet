import { describe, expect, it } from "vitest";
import { filterNavView, findRunner, flattenNav, moveSelection, reconcileSelection, selectionId } from "./nav";
import { previewFleet } from "./preview";

const view = previewFleet();

const closed = { unlinked: false };

describe("navigation search", () => {
  it("keeps matching runners under their parent and keyboard navigation within the results", () => {
    const filtered = filterNavView(view, "  STUDIO-1 ");
    expect(flattenNav(filtered, new Set(), { unlinked: true }).map((i) => selectionId(i.selection))).toEqual([
      "overview", "queue", "moving", "retiring", "machine:ts:nStudio", "runner:gh:acme-demo:101",
    ]);
    expect(flattenNav(filterNavView(view, "spare"), new Set(), closed).map((i) => selectionId(i.selection)).at(-1)).toBe("add-machine");
    expect(view.machines.find((m) => m.id === "ts:nStudio")?.runners).toHaveLength(3);
  });

  it("finds personal descriptions and keeps all runners when their machine matches", () => {
    const personal = previewFleet();
    personal.machines.find((m) => m.id === "ts:nStudio")!.description = "Office closet, top shelf";
    const result = filterNavView(personal, "CLOSET");
    expect(result.machines.map((m) => [m.id, m.runners.length])).toEqual([["ts:nStudio", 3]]);
    // A tailnet device outside the inventory is not a machine, so it never matches.
    expect(filterNavView(view, "spare").machines).toEqual([]);
    expect(filterNavView(view, "no-such-runner").machines).toEqual([]);
    expect(filterNavView(view, " ")).toBe(view);
  });
});

describe("flattenNav", () => {
  it("lists overview, queue, rollout views, machines with their runners, then Add machine", () => {
    const ids = flattenNav(view, new Set(), closed).map((i) => selectionId(i.selection));
    expect(ids.slice(0, 6)).toEqual(["overview", "queue", "moving", "retiring", "machine:ts:nLinux", "runner:gh:acme-demo:301"]);
    // Without a rollout list the two views are left out.
    expect(flattenNav({ ...view, rollout: [] }, new Set(), closed).map((i) => selectionId(i.selection)).slice(0, 3))
      .toEqual(["overview", "queue", "machine:ts:nLinux"]);
    // Runners declared in the inventory appear under their machine even without GitHub.
    expect(ids).toContain("runner:inv:mac-mini-03:mini-03-01");
    expect(ids).not.toContain("runner:gh:acme-demo:501"); // unlinked group is closed
    expect(ids).not.toContain("machine:ts:nSpare"); // tailnet devices are not machines
    expect(ids.at(-1)).toBe("add-machine");
    expect(flattenNav({ ...view, tailnetDevices: [] }, new Set(), closed).map((i) => selectionId(i.selection))).not.toContain("add-machine");
  });

  it("hides runners of collapsed machines and shows the open groups", () => {
    const ids = flattenNav(view, new Set(["ts:nLinux"]), { unlinked: true }).map((i) => selectionId(i.selection));
    expect(ids).not.toContain("runner:gh:acme-demo:301");
    expect(ids).toContain("runner:gh:acme-demo:501");
  });
});

describe("moveSelection", () => {
  const items = flattenNav(view, new Set(), closed);
  it("steps and clamps", () => {
    expect(moveSelection(items, { kind: "overview" }, -1)).toEqual({ kind: "overview" });
    expect(moveSelection(items, { kind: "overview" }, 1)).toEqual({ kind: "queue" });
    expect(moveSelection(items, { kind: "add-machine" }, 1)).toEqual({ kind: "add-machine" });
  });
});

describe("reconcileSelection", () => {
  it("keeps valid selections and falls back for vanished ones", () => {
    expect(reconcileSelection(view, { kind: "machine", id: "ts:nMini" })).toEqual({ kind: "machine", id: "ts:nMini" });
    expect(reconcileSelection(view, { kind: "machine", id: "ts:nSpare" })).toEqual({ kind: "overview" });
    expect(reconcileSelection(view, { kind: "add-machine" })).toEqual({ kind: "add-machine" });
    expect(reconcileSelection({ ...view, tailnetDevices: [] }, { kind: "add-machine" })).toEqual({ kind: "overview" });
    expect(reconcileSelection(view, { kind: "machine", id: "gone" })).toEqual({ kind: "overview" });
    expect(reconcileSelection(view, { kind: "runner", key: "gh:acme-demo:501" })).toEqual({ kind: "runner", key: "gh:acme-demo:501" });
    expect(reconcileSelection(view, { kind: "runner", key: "gone" })).toEqual({ kind: "overview" });
  });

  it("finds runners on machines and those not linked to one", () => {
    expect(findRunner(view, "gh:acme-demo:101")?.name).toBe("studio-1");
    expect(findRunner(view, "gh:acme-demo:501")?.name).toBe("cloud-gpu-1");
  });
});
