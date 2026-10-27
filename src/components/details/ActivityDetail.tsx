import { useState } from "react";
import type { FleetActivityWindow } from "../../bindings/FleetActivityWindow";
import type { FleetFinishedJob } from "../../bindings/FleetFinishedJob";
import type { FleetJobConclusion } from "../../bindings/FleetJobConclusion";
import type { FleetJobOs } from "../../bindings/FleetJobOs";
import type { FleetView } from "../../bindings/FleetView";
import { useNow } from "../../hooks";
import { formatPercent, formatSeconds, plural, timeAgo } from "../../lib/format";
import type { Tone } from "../../lib/status";
import { useUi } from "../../store";
import { Callout, Card, EmptyLine, Section, Stat, StatStrip, StatusDot } from "../ui";
import { Icon } from "../icons";
import { GitHubLink, List, PageHeader } from "./shared";

const WINDOWS = [{ days: 1, label: "24 h" }, { days: 7, label: "7 d" }, { days: 30, label: "30 d" }];
const OS_LABEL: Record<FleetJobOs, string> = { linux: "Linux", windows: "Windows", macos: "macOS" };
const CONCLUSION: Record<FleetJobConclusion, { label: string; tone: Tone }> = {
  success: { label: "Succeeded", tone: "ok" },
  failure: { label: "Failed", tone: "bad" },
  cancelled: { label: "Cancelled", tone: "muted" },
  other: { label: "Finished", tone: "muted" },
};

const usd = (n: number) => `$${n.toFixed(2)}`;

export function ActivityDetail({ view }: { view: FleetView }) {
  const [days, setDays] = useState(7);
  const a = view.activity;
  const w = a.windows.find((x) => x.days === days);
  return (
    <div>
      <PageHeader
        title="Activity"
        subtitle={a.recordingSince
          ? `Finished self-hosted jobs from scanned repositories, recorded on this Mac since ${new Date(a.recordingSince).toLocaleDateString()} while the app was running.`
          : "Finished self-hosted jobs from scanned repositories, recorded on this Mac while the app is running."}
        actions={<div role="group" aria-label="Time window" className="flex rounded-[6px] border border-line bg-card p-0.5">
          {WINDOWS.map((o) => <button key={o.days} type="button" aria-pressed={days === o.days} onClick={() => setDays(o.days)}
            className={`h-6 rounded-[4px] px-2.5 text-[12.5px] font-medium ${days === o.days ? "bg-selected text-ink" : "text-ink-2 hover:text-ink"}`}>{o.label}</button>)}
        </div>}
      />
      {a.storageError && <Callout tone="warn" title="Job history isn't being saved">{a.storageError}</Callout>}
      {!w || w.jobs === 0
        ? <Card className="mt-8 px-6 py-16 text-center text-[13px] text-ink-2">
          No finished jobs recorded in this window. Jobs are recorded when the app sees them queued or running on a self-hosted runner.
        </Card>
        : <WindowSummary w={w} ratesAsOf={a.ratesAsOf} />}
      <Section title="Recent jobs" count={a.recent.length}>
        <List>
          {a.recent.length === 0 ? <EmptyLine>No finished jobs yet.</EmptyLine> : a.recent.map((job) => <JobRow key={job.id} job={job} />)}
        </List>
      </Section>
    </div>
  );
}

function WindowSummary({ w, ratesAsOf }: { w: FleetActivityWindow; ratesAsOf: string }) {
  const select = useUi((s) => s.select);
  const decided = w.succeeded + w.failed;
  const topRepo = Math.max(1, ...w.repos.map((r) => r.runnerSecs));
  const total = w.hosted.reduce((sum, h) => sum + h.usd, 0);
  return <>
    <div className="mt-8"><StatStrip>
      <Stat label="Jobs" value={w.jobs} sub={`${formatSeconds(w.recordedSecs)} recorded`} />
      <Stat label="Success rate" value={decided ? formatPercent(w.succeeded / decided * 100) : "—"} sub={`${w.failed} failed`} />
      <Stat label="Runner time" value={formatSeconds(w.runnerSecs)} sub="time runners spent on jobs" />
      <Stat label="Queue wait" value={w.queueMedianSecs == null ? "—" : formatSeconds(w.queueMedianSecs)} sub={w.queueP95Secs == null ? "median" : `median · p95 ${formatSeconds(w.queueP95Secs)}`} />
    </StatStrip></div>

    <Section title="Busy by machine" count={w.machines.length}>
      <Card className="divide-y divide-line">
        {w.machines.length === 0 && <EmptyLine>No machine has runners GitHub lists right now.</EmptyLine>}
        {w.machines.map((m) => (
          <div key={m.machineId} className="grid grid-cols-[minmax(120px,200px)_1fr_56px] items-center gap-4 px-4 py-2.5 text-[13px]">
            <button className="min-w-0 truncate text-left font-medium hover:underline" onClick={() => select({ kind: "machine", id: m.machineId })}>{m.name}</button>
            <div title={`${formatSeconds(m.busySecs)} busy across ${plural(m.runners, "runner")}`}><Bar fraction={(m.busyPercent ?? 0) / 100} /></div>
            <span className="text-right text-ink-2 tabular">{m.busyPercent == null ? "—" : formatPercent(m.busyPercent)}</span>
          </div>
        ))}
      </Card>
      <p className="mt-2 text-[12px] text-ink-2">Share of each machine's runner capacity (runners GitHub lists now) spent on jobs from scanned repositories during the {formatSeconds(w.recordedSecs)} the app was recording. Time it wasn't running doesn't count as idle.</p>
    </Section>

    <div className="grid grid-cols-1 gap-x-6 min-[1100px]:grid-cols-2">
      <Section title="Top repositories" count={w.repos.length}>
        <Card className="divide-y divide-line">
          {w.repos.map((r) => (
            <div key={r.repo} className="px-4 py-2.5 text-[13px]">
              <div className="flex items-baseline gap-2">
                <span className="min-w-0 flex-1 truncate font-mono text-[12px]">{r.repo}</span>
                <span className="text-[12px] text-ink-2 tabular">{formatSeconds(r.runnerSecs)} · {plural(r.jobs, "job")}</span>
              </div>
              <div className="mt-1.5"><Bar fraction={r.runnerSecs / topRepo} /></div>
            </div>
          ))}
        </Card>
      </Section>
      <Section title="Estimated GitHub-hosted cost">
        <Card className="divide-y divide-line text-[13px]">
          {w.hosted.map((h) => (
            <div key={h.os} className="flex items-baseline gap-3 px-4 py-2.5">
              <span className="w-20">{OS_LABEL[h.os]}</span>
              <span className="min-w-0 flex-1 text-[12px] text-ink-2 tabular">{h.minutes.toLocaleString()} min × ${h.usdPerMinute.toFixed(3)}/min</span>
              <span className="tabular">{usd(h.usd)}</span>
            </div>
          ))}
          <div className="flex items-baseline gap-3 px-4 py-2.5 font-medium"><span className="flex-1">Estimate</span><span className="tabular">{usd(total)}</span></div>
        </Card>
        <p className="mt-2 text-[12px] text-ink-2">An estimate, not a bill: these jobs' minutes, each rounded up, at GitHub's standard hosted-runner rates as of {ratesAsOf}. Jobs without an OS label are priced as Linux; included minutes and larger runners aren't considered.</p>
      </Section>
    </div>
  </>;
}

function Bar({ fraction }: { fraction: number }) {
  return <div className="h-1.5 w-full overflow-hidden rounded-full bg-sunken">
    <div className="h-full rounded-full bg-busy" style={{ width: `${Math.max(0, Math.min(1, fraction)) * 100}%` }} />
  </div>;
}

function JobRow({ job }: { job: FleetFinishedJob }) {
  const now = useNow();
  const c = CONCLUSION[job.conclusion];
  const ran = job.startedAt ? (Date.parse(job.completedAt) - Date.parse(job.startedAt)) / 1000 : null;
  return (
    <div className="flex min-h-[52px] items-center gap-3 border-b border-line px-4 py-2.5 last:border-b-0">
      <span title={c.label}><StatusDot tone={c.tone} /></span>
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate font-medium" title={job.name}>{job.name}</span>
          {job.htmlUrl && <GitHubLink url={job.htmlUrl} className="shrink-0 text-ink-2 hover:text-accent" label={`Open ${job.name} on GitHub`}><Icon name="external" size={12} /></GitHubLink>}
        </div>
        <div className="mt-0.5 truncate font-mono text-[12px] text-ink-2">{job.workflowName && <>{job.workflowName} · </>}{job.repo}</div>
      </div>
      <div className="shrink-0 text-right text-[12px] text-ink-2 tabular">
        <div>{job.runnerName ?? "—"}{ran != null && <> · {formatSeconds(Math.max(0, ran))}</>}</div>
        <div title={new Date(job.completedAt).toLocaleString()}>{c.label} {timeAgo(job.completedAt, now)}</div>
      </div>
    </div>
  );
}
