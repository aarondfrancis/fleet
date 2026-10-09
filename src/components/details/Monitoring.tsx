import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import type { FleetMachineAlert } from "../../bindings/FleetMachineAlert";
import type { FleetMachineEvent } from "../../bindings/FleetMachineEvent";
import type { FleetView } from "../../bindings/FleetView";
import type { MachineView } from "../../bindings/MachineView";
import { useNow } from "../../hooks";
import { formatClock, timeAgo } from "../../lib/format";
import { eventStyle, HISTORY_PAGE_SIZE, mergeEvents, monitoringMachine } from "../../lib/monitoring";
import { commands, isLive } from "../../lib/tauri";
import { useUi } from "../../store";
import { Button, Callout, Card, calloutSurface, EmptyLine, ErrorText, Pill, Section, StatusDot } from "../ui";
import { Icon } from "../icons";

export function MonitoringError({ view }: { view: FleetView }) {
  if (!view.monitoring.storageError) return null;
  return <Callout tone="warn" title="Machine history isn't being saved">
    {view.monitoring.storageError}
    <p className="mt-1">Desktop alerts wait until history can be saved.</p>
  </Callout>;
}

export function ActiveAlerts({ alerts, view, showMachine = false, embedded = false }: { alerts: FleetMachineAlert[]; view: FleetView; showMachine?: boolean; embedded?: boolean }) {
  const now = useNow();
  const select = useUi((s) => s.select);
  const Container = embedded ? "div" : Card;
  return <Container className={`divide-y divide-warn/15 overflow-hidden ${calloutSurface.warn} ${embedded ? "border-t" : ""}`}>
    {alerts.map((alert) => {
      const machine = monitoringMachine(view, alert.machineKey);
      return <div key={alert.id} className="flex items-start gap-3 px-4 py-3">
        <span className="mt-0.5 text-warn"><Icon name="alert" /></span>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2 text-[13px] font-medium">
            {showMachine && (machine
              ? <button className="text-left hover:underline" onClick={() => select({ kind: "machine", id: machine.id })}>{machine.displayName}</button>
              : <span>{alert.machineName}</span>)}
            <span>{alert.kind === "offline" ? "Offline" : "Low disk space"}</span>
            {alert.muted && <Pill tone="muted">Notifications paused</Pill>}
            {alert.stale && <Pill tone="muted" hollow>Awaiting fresh data</Pill>}
          </div>
          <p className="mt-1 whitespace-pre-line break-words text-[12px] text-ink-2">{alert.detail}</p>
          <p className="mt-1 text-[11px] text-ink-3" title={new Date(alert.since).toLocaleString()}>First observed {timeAgo(alert.since, now)}</p>
        </div>
      </div>;
    })}
  </Container>;
}

export function MachineMonitoring({ machine, view }: { machine: MachineView; view: FleetView }) {
  const now = useNow();
  const qc = useQueryClient();
  const until = machine.monitoring.maintenanceUntil;
  const pausedUntil = until && Date.parse(until) > now ? until : null;
  const maintenance = useMutation({
    mutationFn: () => commands.setMachineMaintenance(machine.id, !pausedUntil),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["fleet"] }),
  });
  const disabled = machine.connection.kind === "disabled";
  const alerts = machine.monitoring.alerts;
  return <>
    <Section title="Monitoring">
      <Card className="overflow-hidden">
        <div className="flex flex-wrap items-start gap-3 px-4 py-3">
          <div className="min-w-0 flex-1 basis-[260px]">
            <p className="text-[12px] text-ink-2">
              {pausedUntil ? `Desktop notifications paused until ${formatClock(pausedUntil)}. History and alerts continue.`
                : disabled ? "Monitoring is disabled for this machine in the inventory."
                : "Alerts after 3 minutes offline or confirmed disk space below 10%. Maintenance pauses desktop notifications."}
            </p>
            {maintenance.isError && <ErrorText error={maintenance.error} className="mt-2" />}
          </div>
          {isLive(view) && !disabled && <Button small disabled={maintenance.isPending} onClick={() => maintenance.mutate()}>
            {maintenance.isPending ? "Saving…" : pausedUntil ? "End maintenance" : "Maintenance for 1 hour"}
          </Button>}
        </div>
        {alerts.length > 0 ? <ActiveAlerts alerts={alerts} view={view} embedded />
          : !disabled && <div className="border-t border-line"><EmptyLine>No active alerts.</EmptyLine></div>}
      </Card>
      <MonitoringError view={view} />
    </Section>
    <EventTimeline key={`${view.source}:${machine.monitoring.machineKey}`} view={view} machineKey={machine.monitoring.machineKey} latest={machine.monitoring.events} />
  </>;
}

/** Freeze a contiguous history snapshot while paging; new events get a separate refresh action. */
export function EventTimeline({ latest, view, machineKey = null }: { latest: FleetMachineEvent[]; view: FleetView; machineKey?: string | null }) {
  const now = useNow();
  const select = useUi((s) => s.select);
  const [snapshot, setSnapshot] = useState<FleetMachineEvent[] | null>(null);
  const [shown, setShown] = useState(8);
  const [ended, setEnded] = useState(false);
  const events = snapshot ?? latest;
  const live = isLive(view);
  const more = useMutation({
    mutationFn: (before: string) => commands.getMonitoringEvents(machineKey, before),
    onSuccess: (page) => {
      setSnapshot((old) => mergeEvents(old ?? [], page));
      setShown((count) => count + page.length);
      setEnded(page.length < HISTORY_PAGE_SIZE);
    },
  });
  const hasMore = shown < events.length || (live && !ended && events.length >= HISTORY_PAGE_SIZE);
  function older() {
    if (!snapshot) setSnapshot(events);
    if (shown < events.length) setShown(events.length);
    else if (events.length) more.mutate(events[events.length - 1].id);
  }
  const newEvents = snapshot && latest[0]?.id && latest[0].id !== snapshot[0]?.id;
  return <Section title="Event history" action={newEvents && <Button small variant="ghost" disabled={more.isPending} onClick={() => {
    setSnapshot(null); setShown(8); setEnded(false); more.reset();
  }}>Show latest events</Button>}>
    <p className="mb-3 text-[12px] text-ink-2">Observations while the app is running. Saved on this Mac for 30 days, up to 5,000 events.</p>
    <Card className="divide-y divide-line overflow-hidden">
      {events.length === 0 && <EmptyLine>No events yet. History starts with the first successful observation.</EmptyLine>}
      {events.slice(0, shown).map((event) => {
        const style = eventStyle[event.kind];
        const machine = monitoringMachine(view, event.machineKey);
        return <div key={event.id} className="flex items-start gap-3 px-4 py-3">
          <span className="mt-1.5"><StatusDot tone={style.tone} /></span>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1 text-[12.5px]">
              <span className="font-medium">{style.label}</span>
              {!machineKey && (machine
                ? <button className="text-ink-2 hover:underline" onClick={() => select({ kind: "machine", id: machine.id })}>{event.machineName}</button>
                : <span className="text-ink-2">{event.machineName}</span>)}
              <time dateTime={event.at} title={new Date(event.at).toLocaleString()} className="ml-auto whitespace-nowrap text-[11px] text-ink-3">{timeAgo(event.at, now)}</time>
            </div>
            <p className="mt-1 break-words text-[12px] text-ink-2 selectable">{event.detail}</p>
          </div>
        </div>;
      })}
    </Card>
    {more.isError && <ErrorText error={more.error} className="mt-2" />}
    {hasMore && <div className="mt-2"><Button small variant="ghost" disabled={more.isPending} onClick={older}>{more.isPending ? "Loading…" : "Show older events"}</Button></div>}
  </Section>;
}
