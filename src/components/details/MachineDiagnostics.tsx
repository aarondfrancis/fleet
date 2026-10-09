import type { DiagnosticGroup } from "../../bindings/DiagnosticGroup";
import type { DiagnosticStatus } from "../../bindings/DiagnosticStatus";
import type { MachineView } from "../../bindings/MachineView";
import { useNow } from "../../hooks";
import { diagnosticSummary } from "../../lib/diagnostics";
import { timeAgo } from "../../lib/format";
import type { Tone } from "../../lib/status";
import { Card, Pill, Section, StatusDot } from "../ui";

const GROUPS: [DiagnosticGroup, string][] = [["network", "Tailscale & access"], ["power", "Power & availability"], ["system", "System"], ["runners", "Runner services"]];
const STATUS: Record<DiagnosticStatus, { label: string; tone: Tone }> = {
  pass: { label: "OK", tone: "ok" },
  warn: { label: "Warning", tone: "warn" },
  info: { label: "Info", tone: "muted" },
  unknown: { label: "Unknown", tone: "muted" },
};

export function MachineDiagnostics({ machine: m }: { machine: MachineView }) {
  const now = useNow();
  const d = m.diagnostics;
  return <Section title="Machine diagnostics" action={<span className="text-[12px] text-ink-2">{diagnosticSummary(m)}</span>}>
    <p className={`mb-3 text-[12px] ${d?.stale ? "text-warn" : "text-ink-2"}`}>
      {d ? `${d.stale ? "Last successful read" : "Checked"} ${timeAgo(d.asOf, now)}. ${d.stale ? "These findings may have changed. " : ""}Read automatically over SSH; Check now refreshes them.`
        : m.connection.kind === "disabled" ? "Automatic inspection is disabled for this machine in the inventory."
        : "Diagnostics will appear after a successful SSH check. They also work on machines without runners."}
    </p>
    {d && <div className="grid grid-cols-1 items-start gap-3 min-[1100px]:grid-cols-2">
      {GROUPS.map(([group, title]) => {
        const checks = d.checks.filter((c) => c.group === group);
        if (!checks.length) return null;
        return <Card key={group} className="min-w-0 overflow-hidden">
          <div className="border-b border-line bg-sunken/40 px-4 py-2.5 text-[12px] font-medium">{title}</div>
          <dl className="divide-y divide-line">
            {checks.map((c) => {
              const status = STATUS[c.status];
              return <div key={c.id} className={`px-4 py-3 ${d.stale ? "opacity-60" : ""}`}>
                <div className="flex flex-wrap items-start justify-between gap-2">
                  <dt className="flex min-w-0 items-center gap-2 text-[12px] text-ink-2"><StatusDot tone={d.stale ? "muted" : status.tone} hollow={c.status === "unknown" || d.stale} /><span className="break-words">{c.title}</span></dt>
                  <dd className="max-w-full break-words text-[12px] font-medium">{c.value}</dd>
                </div>
                <dd className="mt-1.5 break-words text-[11.5px] leading-relaxed text-ink-2 selectable">
                  {c.status === "warn" && !d.stale && <span className="mr-2"><Pill tone="warn">Warning</Pill></span>}{c.detail}
                </dd>
              </div>;
            })}
          </dl>
        </Card>;
      })}
    </div>}
  </Section>;
}
