import type { MachineView } from "../../bindings/MachineView";
import { useNow } from "../../hooks";
import { timeAgo } from "../../lib/format";
import { busySince, runnerAsOf, runnerStatus } from "../../lib/status";
import { useUi } from "../../store";
import { LinkRow, LinkTable } from "../Table";
import { Elapsed, EmptyLine, Label, Pill } from "../ui";
import { List } from "./shared";

const COLUMNS = [
  { label: "Runner" },
  { label: "Status" },
  { label: "Labels" },
  { label: "Activity", className: "px-3 text-right" },
];

export function RunnerTable({ m }: { m: MachineView }) {
  const select = useUi((s) => s.select);
  const now = useNow();
  if (!m.runners.length) return <List><EmptyLine>{m.desired ? "The inventory declares no runners here." : "No runners on this machine."}</EmptyLine></List>;
  return (
    <LinkTable columns={COLUMNS} minWidth="min-w-[560px]">
      {m.runners.map((r) => {
        const status = runnerStatus(r);
        const since = busySince(r);
        const asOf = runnerAsOf(r) ?? r.github?.asOf;
        const labels = r.desired?.desiredLabels ?? r.github?.labels ?? [];
        return (
          <LinkRow
            key={r.key}
            onOpen={() => select({ kind: "runner", key: r.key })}
            title={r.name}
            opener={<span className="min-w-0 truncate font-mono text-[12px] font-medium">{r.name}</span>}
          >
            <td className="px-3 py-2.5"><Pill tone={status.tone} hollow={status.hollow}>{status.label}</Pill></td>
            <td className="px-3 py-2.5"><div className="flex max-w-[240px] flex-wrap gap-1">{labels.map((l) => <Label key={l}>{l}</Label>)}</div></td>
            <td className="max-w-[220px] px-3 py-2.5 text-right text-[12px] text-ink-2 tabular">
              {r.currentJob ? <><div className="truncate text-ink" title={r.currentJob.name}>{r.currentJob.workflowName ?? r.currentJob.name}</div>{since && <Elapsed since={since} />}</>
                : r.activity.kind === "busy" ? "Job details unavailable"
                : asOf ? `seen ${timeAgo(asOf, now)}${status.hollow ? " (stale)" : ""}` : "Not checked yet"}
            </td>
          </LinkRow>
        );
      })}
    </LinkTable>
  );
}
