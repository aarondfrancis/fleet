import { describe, expect, it } from "vitest";
import { diagnosticSummary, diagnosticWarnings } from "./diagnostics";
import { previewFleet } from "./preview";
import { hasTailscaleSsh } from "./status";

function machine() {
  const view = previewFleet();
  const machine = view.machines.find((m) => m.displayName === "studio")!;
  machine.diagnostics = {
    asOf: view.generatedAt, stale: false,
    checks: [
      { id: "power-sleep", group: "power", title: "Sleep on AC power", status: "warn", value: "After 10 minutes", detail: "Jobs stop." },
      { id: "tailscale-ssh", group: "network", title: "Tailscale SSH", status: "unknown", value: "Unknown", detail: "Permission denied." },
    ],
  };
  return machine;
}

describe("machine diagnostics", () => {
  it("surfaces only confirmed, current warnings and preserves unknown counts", () => {
    const m = machine();
    expect(diagnosticWarnings(m).map((c) => c.id)).toEqual(["power-sleep"]);
    expect(diagnosticSummary(m)).toBe("1 warning · 1 unknown");
    m.diagnostics!.stale = true;
    expect(diagnosticWarnings(m)).toEqual([]);
    expect(diagnosticSummary(m)).toBe("Stale");
    m.diagnostics!.stale = false;
    m.connection = { kind: "disabled" };
    expect(diagnosticWarnings(m)).toEqual([]);
  });

  it("does not equate the desired SSH transport with an enabled server", () => {
    const m = machine();
    m.tailnet!.sshHostKeys = [];
    m.desired!.host.sshTransport = "tailscale";
    expect(hasTailscaleSsh(m)).toBe(false);
    m.diagnostics!.checks[1] = { ...m.diagnostics!.checks[1], status: "pass", value: "Enabled" };
    expect(hasTailscaleSsh(m)).toBe(true);
    m.diagnostics!.checks[1].value = "Disabled";
    m.tailnet!.sshHostKeys = ["advertised-key"];
    expect(hasTailscaleSsh(m)).toBe(false);
    m.diagnostics!.stale = true;
    expect(hasTailscaleSsh(m)).toBe(true);
  });
});
