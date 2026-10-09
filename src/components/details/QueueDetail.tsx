import type { FleetView } from "../../bindings/FleetView";
import type { QueueView } from "../../bindings/QueueView";
import { useNow } from "../../hooks";
import { plural, timeAgo } from "../../lib/format";
import { emitEvent, isTauri } from "../../lib/tauri";
import { Button, Card, Elapsed, EmptyLine, Label, Pill } from "../ui";
import { Icon } from "../icons";
import { GitHubLink, List, PageHeader } from "./shared";
import { CopyValue } from "../CopyValue";

/** What an empty queue means: nothing waiting, or no job data yet. */
function emptyQueueText(q: QueueView): string {
  return q.asOf ? "Nothing queued for self-hosted runners." : "Waiting for job data from GitHub.";
}

export function QueueDetail({ view }: { view: FleetView }) {
  const now = useNow();
  const q = view.queue;
  return (
    <div>
      <PageHeader title="Queued jobs" subtitle={q.asOf ? `Self-hosted jobs waiting for a runner · checked ${timeAgo(q.asOf, now)}` : "Jobs waiting for a self-hosted runner across scanned repos."} />
      <div className="mt-8">
        {q.jobs.length ? <QueueList view={view} /> : <Card className="flex flex-col items-center gap-3 px-6 py-16 text-center">
          <span className="flex size-10 items-center justify-center rounded-lg border border-line bg-sunken text-ink-2"><Icon name="inbox" size={20} /></span>
          <div className="space-y-1">
            <p className="text-[14px] font-medium">{emptyQueueText(q)}</p>
            <p className="max-w-sm text-[13px] leading-relaxed text-ink-2">{!view.orgs.length ? "Connect a GitHub organization to see queued and in-progress jobs in real time." : q.complete ? "Your scanned repositories have no jobs waiting for a runner." : "Waiting for a complete job scan from GitHub. Check organization access and scan scope in Settings."}</p>
          </div>
          {!view.orgs.length && <Button small variant="primary" disabled={!isTauri} onClick={() => emitEvent("open-settings")}>Add organization</Button>}
        </Card>}
      </div>
      <div className="mt-4 space-y-1.5 text-[12px] text-ink-2">
        {q.otherQueued > 0 && <p>{plural(q.otherQueued, "queued job")} target GitHub-hosted labels (like ubuntu-latest) and are not shown.</p>}
        {!q.complete && <p className="text-warn">This list may be incomplete: job data is missing, stale, or limited to the repositories in each organization's scan scope (Settings).</p>}
        <p>"Label-compatible" means the runner has every label the job asks for. Runner-group access rules can still keep a job off a runner; GitHub's API doesn't expose that per job.</p>
      </div>
    </div>
  );
}

export function QueueList({ view, limit, compact }: { view: FleetView; limit?: number; compact?: boolean }) {
  const jobs = limit ? view.queue.jobs.slice(0, limit) : view.queue.jobs;
  return (
    <List>
      {jobs.length === 0 ? <EmptyLine><span className="flex items-center gap-2.5"><Icon name="inbox" size={16} />{emptyQueueText(view.queue)}</span></EmptyLine> : jobs.map((q) => (
        <div key={q.job.id} className={`flex min-h-[56px] gap-3 border-b border-line px-4 py-3 last:border-b-0 ${compact ? "flex-col" : "flex-wrap items-center"}`}>
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <span className="min-w-0 flex-1 truncate font-medium" title={q.job.name}>{q.job.name}</span>
              {q.job.htmlUrl && <GitHubLink url={q.job.htmlUrl} className="shrink-0 text-ink-2 hover:text-accent" label={`Open ${q.job.name} on GitHub`}><Icon name="external" size={12} /></GitHubLink>}
            </div>
            <div className="mt-0.5 truncate font-mono text-[12px] text-ink-2">{q.job.workflowName && <>{q.job.workflowName} · </>}<CopyValue value={q.job.repo} label="Repository" /></div>
            <div className="mt-1.5 flex flex-wrap gap-1">{q.job.labels.map((l) => <Label key={l}>{l}</Label>)}</div>
          </div>
          <div className={`flex shrink-0 gap-2 ${compact ? "flex-wrap items-center" : "flex-col items-end"}`}>
            {q.compatibleRunners.length === 0 ? <Pill tone="warn">No matching runner</Pill>
              : q.compatibleOnline === 0 ? <Pill tone="warn">Matching runners offline</Pill>
              : q.compatibleIdle > 0 ? <Pill tone="ok">{plural(q.compatibleIdle, "idle runner")} match</Pill>
              : <Pill tone="busy">{q.compatibleOnline} matching, all working</Pill>}
            {q.createdAt && <span className="text-[11px] text-ink-2 tabular">waiting <Elapsed since={q.createdAt} /></span>}
          </div>
        </div>
      ))}
      {limit && view.queue.jobs.length > limit && <EmptyLine>and {view.queue.jobs.length - limit} more in Queued jobs</EmptyLine>}
    </List>
  );
}
