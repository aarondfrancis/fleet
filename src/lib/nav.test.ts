import { describe, expect, it } from "vitest";
import { filterNavView, findRunner, flattenNav, moveSelection, reconcileSelection, selectionId } from "./nav";
import { previewFleet } from "./preview";

const view = previewFleet();

const closed = { unlinked: false, other: false };

describe("navigation search", () => {
  it("keeps matching runners under their parent and keyboard navigation within the results", () => {
    const filtered = filterNavView(view, "  STUDIO-1 ");
    expect(flattenNav(filtered, new Set(), { other: true, unlinked: true }).map((i) => selectionId(i.selection))).toEqual([
      "overview", "queue", "activity", "machine:ts:nStudio", "runner:gh:acme-demo:101",
    ]);
    expect(view.machines.find((m) => m.id === "ts:nStudio")?.runners).toHaveLength(3);
  });

  it("finds personal descriptions and keeps all runners when their machine matches", () => {
    const personal = previewFleet();
    personal.machines.find((m) => m.id === "ts:nStudio")!.description = "Office closet, top shelf";
    const result = filterNavView(personal, "CLOSET");
    expect(result.machines.map((m) => [m.id, m.runners.length])).toEqual([["ts:nStudio", 3]]);
    expect(filterNavView(view, "spare").otherMachines.map((m) => m.id)).toEqual(["ts:nSpare"]);
    expect(filterNavView(view, "no-such-runner").machines).toEqual([]);
    expect(filterNavView(view, " ")).toBe(view);
  });
});

describe("flattenNav", () => {
  it("lists overview, queue, activity, machines with their runners, then ignored", () => {
    const ids = flattenNav(view, new Set(), closed).map((i) => selectionId(i.selection));
    expect(ids.slice(0, 5)).toEqual(["overview", "queue", "activity", "machine:ts:nLinux", "runner:gh:acme-demo:301"]);
    // Runners declared in the inventory appear under their machine even without GitHub.
    expect(ids).toContain("runner:inv:mac-mini-03:mini-03-01");
    expect(ids).not.toContain("runner:gh:acme-demo:501"); // unlinked group is closed
    expect(ids).not.toContain("machine:ts:nSpare"); // other machines are closed
    expect(ids.at(-1)).toBe("ignored");
  });

  it("hides runners of collapsed machines and shows the open groups", () => {
    const ids = flattenNav(view, new Set(["ts:nLinux"]), { unlinked: true, other: true }).map((i) => selectionId(i.selection));
    expect(ids).not.toContain("runner:gh:acme-demo:301");
    expect(ids).toContain("runner:gh:acme-demo:501");
    expect(ids).toContain("machine:ts:nSpare");
  });
});

describe("moveSelection", () => {
  const items = flattenNav(view, new Set(), closed);
  it("steps and clamps", () => {
    expect(moveSelection(items, { kind: "overview" }, -1)).toEqual({ kind: "overview" });
    expect(moveSelection(items, { kind: "overview" }, 1)).toEqual({ kind: "queue" });
    expect(moveSelection(items, { kind: "ignored" }, 1)).toEqual({ kind: "ignored" });
  });
});

describe("reconcileSelection", () => {
  it("keeps valid selections and falls back for vanished ones", () => {
    expect(reconcileSelection(view, { kind: "machine", id: "ts:nMini" })).toEqual({ kind: "machine", id: "ts:nMini" });
    expect(reconcileSelection(view, { kind: "machine", id: "ts:nSpare" })).toEqual({ kind: "machine", id: "ts:nSpare" });
    expect(reconcileSelection(view, { kind: "machine", id: "gone" })).toEqual({ kind: "overview" });
    expect(reconcileSelection(view, { kind: "runner", key: "gh:acme-demo:501" })).toEqual({ kind: "runner", key: "gh:acme-demo:501" });
    expect(reconcileSelection(view, { kind: "runner", key: "gone" })).toEqual({ kind: "overview" });
  });

  it("finds runners on machines and those not linked to one", () => {
    expect(findRunner(view, "gh:acme-demo:101")?.name).toBe("studio-1");
    expect(findRunner(view, "gh:acme-demo:501")?.name).toBe("cloud-gpu-1");
  });
});
