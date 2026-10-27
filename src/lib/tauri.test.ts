import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const { invoke, emit, listen } = vi.hoisted(() => ({ invoke: vi.fn(), emit: vi.fn(), listen: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ emit, listen }));

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  vi.stubGlobal("window", { __TAURI_INTERNALS__: {} });
  invoke.mockResolvedValue(undefined);
  emit.mockResolvedValue(undefined);
});
afterEach(() => vi.unstubAllGlobals());

describe("configuration notifications", () => {
  it("broadcasts every configuration mutation only after its successful save", async () => {
    const { commands } = await import("./tauri");
    const mutations = [
      () => commands.addOrg("example", "ghCli", null, { kind: "organization", repoScope: { kind: "none" }, runnerRepos: [] }),
      () => commands.useGhForOrg("example"),
      () => commands.updateOrgToken("example", "fake"),
      () => commands.setOrgScope("example", { kind: "none" }, []),
      () => commands.removeOrg("example"),
      () => commands.ignorePeer("fake"),
      () => commands.unignorePeer("fake"),
      () => commands.setMachinePreferences("fake", "Name", "Note"),
      () => commands.setDemoMode(true),
      () => commands.setDesktopNotifications(true),
      () => commands.setLocalApi(true),
    ];
    for (const mutation of mutations) {
      emit.mockClear();
      invoke.mockClear();
      let saved!: () => void;
      invoke.mockReturnValueOnce(new Promise<void>((resolve) => { saved = resolve; }));
      const pending = mutation();
      await vi.waitFor(() => expect(invoke).toHaveBeenCalled());
      expect(emit).not.toHaveBeenCalled();
      saved();
      await pending;
      expect(emit).toHaveBeenCalledExactlyOnceWith("config-updated");
    }
  });

  it("does not invalidate settings after a failed save or a fleet/read command", async () => {
    const { commands } = await import("./tauri");
    invoke.mockRejectedValueOnce(new Error("save failed"));
    await expect(commands.setDemoMode(true)).rejects.toThrow("save failed");
    await commands.getFleet();
    await commands.refreshNow();
    await commands.setMachineMaintenance("fake", true);
    expect(emit).not.toHaveBeenCalled();
  });

  it("listens across Tauri windows and returns the native teardown", async () => {
    const { onConfigUpdated } = await import("./tauri");
    const off = vi.fn();
    const handler = vi.fn();
    listen.mockResolvedValueOnce(off);
    expect(await onConfigUpdated(handler)).toBe(off);
    expect(listen).toHaveBeenCalledWith("config-updated", handler);
  });

  it("supports preview notifications and removes preview listeners", async () => {
    vi.stubGlobal("window", {});
    const { onConfigUpdated, emitEvent } = await import("./tauri");
    const handler = vi.fn();
    const off = await onConfigUpdated(handler);
    await emitEvent("config-updated");
    expect(handler).toHaveBeenCalledOnce();
    off();
    await emitEvent("config-updated");
    expect(handler).toHaveBeenCalledOnce();
  });
});
