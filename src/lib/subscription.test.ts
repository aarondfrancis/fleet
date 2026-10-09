import { describe, expect, it, vi } from "vitest";
import { subscribeToUpdates } from "./subscription";

describe("async event subscription lifecycle", () => {
  it("ignores late events and removes a listener registered after unmount", async () => {
    let handler!: (value: number) => void;
    let ready!: (off: () => void) => void;
    const off = vi.fn();
    const update = vi.fn();
    const refresh = vi.fn();
    const cleanup = subscribeToUpdates<number>((callback) => {
      handler = callback;
      return new Promise((resolve) => { ready = resolve; });
    }, update, refresh);
    cleanup();
    handler(42);
    ready(off);
    await Promise.resolve();
    expect(off).toHaveBeenCalledOnce();
    expect(update).not.toHaveBeenCalled();
    expect(refresh).not.toHaveBeenCalled();
  });

  it("refreshes after registration, forwards updates, and cleans up once", async () => {
    let handler!: (value: number) => void;
    const off = vi.fn();
    const update = vi.fn();
    const refresh = vi.fn();
    const cleanup = subscribeToUpdates<number>(async (callback) => {
      handler = callback;
      return off;
    }, update, refresh);
    await Promise.resolve();
    expect(refresh).toHaveBeenCalledOnce();
    handler(5);
    expect(update).toHaveBeenCalledWith(5);
    cleanup();
    cleanup();
    handler(10);
    expect(off).toHaveBeenCalledOnce();
    expect(update).toHaveBeenCalledOnce();
  });

  it("handles registration failure without an unhandled rejection", async () => {
    const error = new Error("listener failed");
    const report = vi.fn();
    subscribeToUpdates(async () => { throw error; }, vi.fn(), vi.fn(), report);
    await Promise.resolve();
    await Promise.resolve();
    expect(report).toHaveBeenCalledWith(error);
  });
});
