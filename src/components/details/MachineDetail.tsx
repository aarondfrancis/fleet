import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import type { FleetView } from "../../bindings/FleetView";
import type { MachineView } from "../../bindings/MachineView";
import { useNow } from "../../hooks";
import { operationRunning, requestTargetsMachine } from "../../lib/desired";
import { timeAgo } from "../../lib/format";
import { activityCounts, machineStatus } from "../../lib/status";
import { commands, isLive } from "../../lib/tauri";
import { useUi } from "../../store";
import { Button, Callout, Card, ErrorText, Pill, Section } from "../ui";
import { Icon } from "../icons";
import { OperationCard, useChange } from "./changes";
import { ChangePanel } from "./ReviewCard";
import { HealthMeters, PageHeader } from "./shared";
import { MachineChanges } from "./MachineChanges";
import { MachineDiagnostics } from "./MachineDiagnostics";
import { MachineFacts } from "./MachineFacts";
import { MachineGlance } from "./MachineGlance";
import { MachinePreferencesForm } from "../MachinePreferencesForm";
import { MachineMonitoring } from "./Monitoring";
import { RunnerChecks } from "./RunnerChecks";
import { RunnerTable } from "./RunnerTable";
import { CopyValue } from "../CopyValue";

const OS_NAME = { macos: "macOS", linux: "Linux", windows: "Windows", other: "Other" } as const;

export function MachineDetail({ machine: m, view }: { machine: MachineView; view: FleetView }) {
  const [editing, setEditing] = useState(false);
  const change = useChange();
  const st = machineStatus(m.connection);
  const d = m.desired;
  const op = view.plan.operation;
  const busy = operationRunning(view);
  const live = isLive(view);
  const hostname = m.tailnet?.hostname !== m.displayName ? m.tailnet?.hostname : null;
  const address = m.tailnet?.dnsName ?? m.host;
  const os = m.osVersion ?? (m.os ? OS_NAME[m.os] : null);
  const subtitle = <>
    {hostname && <><CopyValue value={hostname} label="Hostname" /> · </>}
    <CopyValue value={address} label="Address" />{os && ` · ${os}`}
  </>;

  return (
    <div>
      <PageHeader
        titleText={m.displayName}
        icon={<Icon name="machine" size={18} />}
        title={<CopyValue value={m.displayName} label="Machine name" showIcon={false} />}
        status={<>
          <Pill tone={st.tone} hollow={st.hollow}>{st.label}</Pill>
          {live && !editing && <Button small variant="ghost" aria-label={`Edit ${m.displayName}`} onClick={() => setEditing(true)}>Edit…</Button>}
        </>}
        subtitle={subtitle}
        actions={live && <IgnoreMachine m={m} view={view} />}
      />
      {editing && live ? <Card className="mt-4 px-4 py-3">
        <p className="text-[12px] text-ink-2">Personal name and description, saved only on this Mac.</p>
        <MachinePreferencesForm machine={m} close={() => setEditing(false)} />
      </Card> : m.description && <p className="mt-3 whitespace-pre-wrap break-words text-[12.5px] text-ink-2 selectable">{m.description}</p>}
      <div className="mt-8 space-y-3"><MachineGlance m={m} />
        {m.health && <HealthMeters h={m.health} />}
      </div>

      {op && requestTargetsMachine(op.request, m) && <OperationCard op={op} />}
      {d && <MachineChanges d={d} change={change} live={live && !busy} />}
      <ChangePanel change={change} busy={busy} />
      <ConnectionNotice m={m} />
      <MachineMonitoring key={`${view.source}:${m.id}`} machine={m} view={view} />
      <MachineDiagnostics machine={m} />

      <Section title="Runners" count={m.runners.length} action={<span className="font-mono text-[12px] text-ink-2">{runnerSummary(m)}</span>}>
        <RunnerTable m={m} />
      </Section>

      <MachineFacts m={m} inFleet={view.machines.some((x) => x.id === m.id)} />
      {d && (
        <RunnerChecks
          host={d.host.id}
          checks={d.checks}
          checkedAt={d.checksAt}
          running={view.plan.doctorRunning}
          live={live}
          notRun="Read-only over SSH: identity, prerequisites, disk and power settings (and auto-logon for logon-task hosts)."
        />
      )}

      {m.warnings.length > 0 && (
        <Section title="SSH check notes" count={m.warnings.length}>
          <Card className="px-4 py-3 text-[12px] text-ink-2 selectable">
            {m.warnings.map((w) => (
              <div key={w}>{w}</div>
            ))}
          </Card>
        </Section>
      )}
    </div>
  );
}

function runnerSummary(m: MachineView): string {
  const c = activityCounts(m.runners);
  return `${c.busy} working · ${c.idle} idle · ${c.offline} offline${c.unknown ? ` · ${c.unknown} unknown` : ""}`;
}

function ConnectionNotice({ m }: { m: MachineView }) {
  const now = useNow();
  const c = m.connection;
  switch (c.kind) {
    case "stale":
      return (
        <Callout tone="warn" title={`Showing the last SSH read, from ${timeAgo(c.checkedAt, now)}`}>
          {c.reason} Health below is not current.
        </Callout>
      );
    case "offline":
      return (
        <Callout tone="warn" title="Offline on the tailnet">
          {c.lastSeen ? `Last seen ${timeAgo(c.lastSeen, now)}.` : "Not seen recently."} Runners show their last known state.
        </Callout>
      );
    case "error":
      // The inventory check reports the same failure above.
      if (m.desired?.error === c.detail) return null;
      return <Callout tone="bad" title="Couldn't read this machine over SSH">{c.detail}</Callout>;
    default:
      return null;
  }
}

/** Other tailnet machines can be hidden from the dashboard. */
function IgnoreMachine({ m, view }: { m: MachineView; view: FleetView }) {
  const select = useUi((s) => s.select);
  const tailnet = m.tailnet;
  const ignore = useMutation({
    mutationFn: (nodeId: string) => commands.ignorePeer(nodeId),
    onSuccess: () => select({ kind: "overview" }),
  });
  if (!tailnet || !view.otherMachines.some((x) => x.id === m.id)) return null;
  return (
    <>
      {ignore.isError && <ErrorText error={ignore.error} />}
      <Button small variant="ghost" disabled={ignore.isPending} onClick={() => ignore.mutate(tailnet.nodeId)}>
        Ignore
      </Button>
    </>
  );
}
