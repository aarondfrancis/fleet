import type { FleetIssue } from "../../bindings/FleetIssue";
import type { FleetIssueCategory } from "../../bindings/FleetIssueCategory";
import type { FleetView } from "../../bindings/FleetView";
import { useNow } from "../../hooks";
import { plural, timeAgo } from "../../lib/format";
import { allMachines, allRunners } from "../../lib/nav";
import { activityCounts } from "../../lib/status";
import { emitEvent, isLive } from "../../lib/tauri";
import { useUi } from "../../store";
import { calloutSurface, Card, EmptyLine, Section, Stat, StatStrip } from "../ui";
import { Icon } from "../icons";
import { OperationCard } from "./changes";
import { List, PageHeader, RunnerLine } from "./shared";
import { MachineTable } from "./MachineTable";
import { QueueList } from "./QueueDetail";
import { ActiveAlerts, EventTimeline, MonitoringError } from "./Monitoring";
import { RunnerChecks } from "./RunnerChecks";
import { Unlinked } from "./Unlinked";
import { Welcome } from "./Welcome";

export function Overview({ view }: { view: FleetView }) {
  const s = view.summary;
  const machines = allMachines(view);
  const runners = allRunners(view);
  const working = runners.filter((r) => r.activity.kind === "busy");
  const idle = activityCounts(runners).idle;

  if (view.orgs.length === 0 && machines.length === 0 && view.source === "live") {
    return <Welcome view={view} />;
  }

  const orgs = view.orgs.map((o) => o.login).join(", ");
  return (
    <div>
      <PageHeader
        title="Fleet overview"
        subtitle={`${plural(machines.length, "machine")} · ${plural(s.runners, "runner")}${orgs ? ` · ${orgs}` : ""}`}
      />
      <PlanLine view={view} />
      {view.plan.operation && <OperationCard op={view.plan.operation} />}
      <div className="mt-8"><StatStrip>
        <Stat label="Working" value={s.runnersBusy} tone="busy" sub={`of ${s.runners} runners`}>
          <div className="flex h-1 overflow-hidden rounded-full bg-sunken" aria-hidden="true">
            <span className="bg-busy" style={{ width: `${s.runners ? s.runnersBusy / s.runners * 100 : 0}%` }} />
            <span className="bg-ok/80" style={{ width: `${s.runners ? idle / s.runners * 100 : 0}%` }} />
          </div>
        </Stat>
        <Stat label="Idle" value={idle} tone="ok" sub="online, ready for jobs" />
        <Stat label="Offline" value={s.runnersOffline} tone="muted" sub={s.runnersUnknown ? `${s.runnersUnknown} unknown` : "per GitHub"} />
        <Stat
          label="Queued jobs"
          value={view.queue.jobs.length}
          sub={view.queue.complete ? "self-hosted, scanned repos" : "partial: some data missing"}
        />
      </StatStrip></div>

      <MonitoringError view={view} />
      {view.monitoring.alerts.length > 0 && <Section title="Active alerts" count={view.monitoring.alerts.length}>
        <ActiveAlerts alerts={view.monitoring.alerts} view={view} showMachine />
      </Section>}
      <Attention issues={view.issues} />

      <div className="grid grid-cols-1 gap-x-6 min-[1100px]:grid-cols-2">
        <Section title="Working now" count={working.length}>
          <List>
            {working.length === 0 ? <EmptyLine>No runner is working right now.</EmptyLine> : working.map((r) => <RunnerLine key={r.key} r={r} view={view} />)}
          </List>
        </Section>
        <Section title="Queued" count={view.queue.jobs.length}>
          <QueueList view={view} limit={5} compact />
        </Section>
      </div>

      <Section title="Machines" count={machines.length} action={
        <div className="flex items-center gap-3 text-[12px] text-ink-2">
          <Legend color="bg-busy">Working</Legend>
          <Legend color="bg-ok">Idle</Legend>
          <Legend color="border border-dashed border-bad">Offline</Legend>
          <Legend color="border border-dashed border-muted">Unknown</Legend>
        </div>
      }>
        <MachineTable machines={machines} />
      </Section>

      {view.unmatchedRunners.length > 0 && <Unlinked view={view} />}
      <EventTimeline key={view.source} view={view} latest={view.monitoring.events} />
      {(view.plan.organization || view.plan.controlChecks.length > 0) && (
        <RunnerChecks
          host={null}
          checks={view.plan.controlChecks}
          checkedAt={view.plan.checksAt}
          running={view.plan.doctorRunning}
          live={isLive(view)}
          notRun="Read-only: GitHub access and the inventory here, and each machine over SSH. Each machine's results appear on its page."
        />
      )}
    </div>
  );
}

/** Findings shown elsewhere on the overview: alerts, history storage and the running change. */
const SHOWN_ELSEWHERE: FleetIssueCategory[] = ["alert", "storage", "operation"];

export function Attention({ issues }: { issues: FleetIssue[] }) {
  const select = useUi((s) => s.select);
  const items = issues
    .filter((issue) => !SHOWN_ELSEWHERE.includes(issue.category))
    .map((issue) => {
      const { runnerKey, machineId } = issue;
      return {
        key: issue.id,
        text: issue.title,
        sub: issue.detail,
        action: issue.fix === "addOrganization" ? "Add organization" : undefined,
        onClick: issue.fix ? () => emitEvent("open-settings")
          : runnerKey ? () => select({ kind: "runner", key: runnerKey })
          : machineId ? () => select({ kind: "machine", id: machineId })
          : issue.category === "queue" ? () => select({ kind: "queue" })
          : undefined,
      };
    });
  if (items.length === 0) return null;
  return (
    <Section title="Needs attention" count={items.length}>
      <Card className={`divide-y divide-warn/15 overflow-hidden ${calloutSurface.warn}`}>
        {items.map((i) => {
          const content = (
            <>
              <span className="text-warn"><Icon name="alert" size={13} /></span>
              <div className="min-w-0 flex-1">
                <div className="truncate font-medium">{i.text}</div>
                <div className="mt-0.5 line-clamp-2 break-words text-[13px] text-ink-2" title={i.sub}>{i.sub}</div>
              </div>
              {i.onClick && (i.action
                ? <span className="flex shrink-0 items-center gap-1.5 rounded-md border border-line bg-card px-2.5 py-1 text-[12.5px] font-medium">{i.action}<Icon name="chevron" size={12} /></span>
                : <span className="text-ink-3"><Icon name="chevron" size={11} /></span>)}
            </>
          );
          const className = "flex min-h-[40px] w-full items-center gap-3 px-4 py-3 text-left";
          return i.onClick
            ? <button key={i.key} type="button" onClick={i.onClick} className={`${className} hover:bg-warn/5`}>{content}</button>
            : <div key={i.key} className={className}>{content}</div>;
        })}
      </Card>
    </Section>
  );
}

function Legend({ color, children }: { color: string; children: React.ReactNode }) {
  return <span className="flex items-center gap-1.5"><span aria-hidden className={`h-3 w-1.5 rounded-[2px] ${color}`} />{children}</span>;
}

/** When the fleet was last compared with the inventory. */
function PlanLine({ view }: { view: FleetView }) {
  const now = useNow();
  const p = view.plan;
  if (!p.organization && !p.inventoryError) return null;
  return (
    <p className="mt-1 break-words text-[12px] text-ink-2">
      {p.refreshing
        ? "Checking GitHub and each machine against the inventory…"
        : p.checkedAt
          ? `Compared with ${p.inventoryPath} ${timeAgo(p.checkedAt, now)}.`
          : "Not compared with the inventory yet. Check now reads GitHub and each machine over SSH; it never changes anything."}
    </p>
  );
}
