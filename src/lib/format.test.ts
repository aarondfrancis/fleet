import { describe, expect, it } from "vitest";
import { elapsedSince, formatBytes, formatElapsed, formatSeconds, formatUptime, timeAgo } from "./format";

describe("formatSeconds", () => {
  it("shows seconds, then minutes, then hours and days", () => {
    expect(formatSeconds(45)).toBe("45s");
    expect(formatSeconds(725)).toBe("12m 5s");
    expect(formatSeconds(3 * 3600 + 12 * 60)).toBe("3h 12m");
    expect(formatSeconds(2 * 86400 + 4 * 3600)).toBe("2d 4h");
  });
});

describe("formatElapsed", () => {
  it("uses m:ss under an hour and h:mm:ss above", () => {
    expect(formatElapsed(0)).toBe("0:00");
    expect(formatElapsed(42_000)).toBe("0:42");
    expect(formatElapsed(12 * 60_000 + 5_000)).toBe("12:05");
    expect(formatElapsed(3_729_000)).toBe("1:02:09");
  });

  it("never goes negative (clock skew)", () => {
    expect(formatElapsed(-5_000)).toBe("0:00");
  });
});

describe("elapsedSince", () => {
  it("times from a timestamp", () => {
    expect(elapsedSince("2026-10-03T12:00:00Z", Date.parse("2026-10-03T12:12:05Z"))).toBe("12:05");
  });
});

describe("timeAgo", () => {
  const now = Date.parse("2026-10-03T12:00:00Z");
  it("buckets", () => {
    expect(timeAgo("2026-10-03T12:00:00Z", now)).toBe("just now");
    expect(timeAgo("2026-10-03T11:59:18Z", now)).toBe("42s ago");
    expect(timeAgo("2026-10-03T11:55:00Z", now)).toBe("5m ago");
    expect(timeAgo("2026-10-03T09:00:00Z", now)).toBe("3h ago");
    expect(timeAgo("2026-09-30T12:00:00Z", now)).toBe("3d ago");
    expect(timeAgo(null, now)).toBe("never");
  });
});

describe("formatBytes", () => {
  it("uses decimal units", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(14_000_000_000)).toBe("14.0 GB");
    expect(formatBytes(312_000_000_000)).toBe("312 GB");
    expect(formatBytes(null)).toBe("—");
  });
});

describe("formatUptime", () => {
  it("is compact", () => {
    expect(formatUptime(9 * 86400 + 4 * 3600)).toBe("9d 4h");
    expect(formatUptime(3 * 3600 + 120)).toBe("3h 2m");
    expect(formatUptime(300)).toBe("5m");
  });
});
