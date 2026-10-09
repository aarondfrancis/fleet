import type { MachineView } from "../../bindings/MachineView";
import { diagnosticSummary, diagnosticWarnings } from "../../lib/diagnostics";
import { formatBytes, formatPercent, formatUptime, timeAgo } from "../../lib/format";
import { diskUsedFraction, memoryUsedFraction, primaryDisk } from "../../lib/health";
import { useNow } from "../../hooks";
import { machineDot, machineSummary, machineTooltip } from "../../lib/status";
import { useUi } from "../../store";
import { LinkRow, LinkTable } from "../Table";
import { EmptyLine, Meter, StatusDot } from "../ui";
import { List, RunnerCells } from "./shared";

const COLUMNS = [
  { label: "Machine" },
  { label: "Runners" },
  { label: "Diagnostics" },
  { label: "CPU", className: "w-28 px-3" },
  { label: "Memory", className: "w-28 px-3" },
  { label: "Disk free", className: "w-28 px-3" },
  { label: "Uptime", className: "hidden px-3 min-[1300px]:table-cell" },
];

export function MachineTable({ machines }: { machines: MachineView[] }) {
  const select = useUi((s) => s.select);
  const now = useNow();
  if (!machines.length) return <List><EmptyLine>No machines yet.</EmptyLine></List>;
  return (
    <LinkTable columns={COLUMNS} minWidth="min-w-[640px]">
      {machines.map((m) => {
        const dot = machineDot(m);
        const h = m.health;
        const memory = memoryUsedFraction(h);
        const disk = primaryDisk(h);
        return (
          <LinkRow
            key={m.id}
            onOpen={() => select({ kind: "machine", id: m.id })}
            title={machineTooltip(m)}
            opener={<>
              <StatusDot tone={dot.tone} hollow={dot.hollow} />
              <span className="min-w-0">
                <span className="block truncate font-medium">{m.displayName}</span>
                <span className="block truncate font-mono text-[11px] text-ink-2">{m.desired?.host.platform ?? m.os ?? m.host}</span>
                {h?.stale && <span className="block text-[11px] text-warn">as of {timeAgo(h.asOf, now)}</span>}
              </span>
            </>}
          >
            <td className="px-3 py-2.5"><div className="flex flex-wrap items-center gap-2"><RunnerCells runners={m.runners} /><span className="whitespace-nowrap font-mono text-[11px] text-ink-2">{machineSummary(m)}</span></div></td>
            <td className={`px-3 py-2.5 text-[12px] ${diagnosticWarnings(m).length ? "text-warn" : "text-ink-2"}`}>{diagnosticSummary(m)}</td>
            <MetricCell value={h?.cpuPercent == null ? "—" : formatPercent(h.cpuPercent)} fraction={h?.cpuPercent == null ? null : h.cpuPercent / 100} stale={h?.stale} />
            <MetricCell value={memory == null ? "—" : formatPercent(memory * 100)} fraction={memory} stale={h?.stale} />
            <MetricCell value={disk ? formatBytes(disk.availableBytes) : "—"} fraction={diskUsedFraction(disk)} stale={h?.stale} />
            <td className="hidden px-3 py-2.5 font-mono text-[12px] text-ink-2 tabular min-[1300px]:table-cell">{h?.uptimeSecs == null ? "—" : formatUptime(h.uptimeSecs)}</td>
          </LinkRow>
        );
      })}
    </LinkTable>
  );
}

function MetricCell({ value, fraction, stale }: { value: string; fraction: number | null; stale?: boolean }) {
  return (
    <td className="px-3 py-2.5">
      <div className="flex min-w-[64px] flex-col gap-1.5">
        <span className={`font-mono text-[12px] tabular ${stale ? "text-ink-3" : "text-ink"}`}>{value}</span>
        {fraction != null && <Meter fraction={fraction} stale={stale} />}
      </div>
    </td>
  );
}
