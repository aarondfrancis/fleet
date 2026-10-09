import type { MachineDiagnostic } from "../bindings/MachineDiagnostic";
import type { MachineView } from "../bindings/MachineView";

/** Old findings remain visible on the machine but do not become current fleet alerts. */
export function diagnosticWarnings(machine: MachineView): MachineDiagnostic[] {
  const diagnostics = machine.diagnostics;
  if (!diagnostics || diagnostics.stale || machine.connection.kind !== "connected") return [];
  return diagnostics.checks.filter((check) => check.status === "warn");
}

export function diagnosticSummary(machine: MachineView): string {
  const diagnostics = machine.diagnostics;
  if (!diagnostics) return "Not checked";
  if (diagnostics.stale) return "Stale";
  const warnings = diagnostics.checks.filter((c) => c.status === "warn").length;
  const unknown = diagnostics.checks.filter((c) => c.status === "unknown").length;
  return [warnings ? `${warnings} warning${warnings === 1 ? "" : "s"}` : null, unknown ? `${unknown} unknown` : null].filter(Boolean).join(" · ") || "No warnings";
}
