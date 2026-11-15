import { useMutation } from "@tanstack/react-query";
import { useEffect, useRef } from "react";
import type { FleetView } from "../bindings/FleetView";
import { findMachine, findRunner, selectionId, type Selection } from "../lib/nav";
import { commands, isLive, isTauri } from "../lib/tauri";
import { useUi } from "../store";
import { ActivityDetail } from "./details/ActivityDetail";
import { IgnoredDetail } from "./details/IgnoredDetail";
import { MachineDetail } from "./details/MachineDetail";
import { Overview } from "./details/Overview";
import { QueueDetail } from "./details/QueueDetail";
import { RunnerDetail } from "./details/RunnerDetail";
import { Icon } from "./icons";
import { Button, ErrorText, TimeAgo } from "./ui";

export function DetailPane({ view, selection }: { view: FleetView; selection: Selection }) {
  const select = useUi((s) => s.select);
  const scrollRef = useRef<HTMLDivElement>(null);
  const current = selectionId(selection);
  useEffect(() => { if (scrollRef.current) scrollRef.current.scrollTop = 0; }, [current]);
  const runner = selection.kind === "runner" ? findRunner(view, selection.key) : undefined;
  const machine = findMachine(view, selection.kind === "machine" ? selection.id : runner?.machineId ?? null);
  // A machine's page compares only its inventory host; one outside the inventory has nothing to compare.
  const inventoryHost = machine ? machine.desired?.host.id : null;
  const check = useMutation({
    mutationFn: async () => {
      await commands.refreshNow();
      if (view.plan.organization && inventoryHost !== undefined) await commands.desiredRefresh(inventoryHost);
    },
  });
  const checking = view.plan.refreshing || check.isPending;
  const crumbs: { label: string; selection?: Selection }[] = [{ label: "Fleet", selection: { kind: "overview" } }];
  if (selection.kind === "overview") crumbs.push({ label: "Fleet overview" });
  if (selection.kind === "queue") crumbs.push({ label: "Queued jobs" });
  if (selection.kind === "activity") crumbs.push({ label: "Activity" });
  if (selection.kind === "ignored") crumbs.push({ label: "Ignored machines" });
  if (machine) crumbs.push({ label: machine.displayName, selection: { kind: "machine", id: machine.id } });
  if (runner) crumbs.push({ label: runner.name });

  let body: React.ReactNode;
  switch (selection.kind) {
    case "overview": body = <Overview view={view} />; break;
    case "queue": body = <QueueDetail view={view} />; break;
    case "activity": body = <ActivityDetail view={view} />; break;
    case "machine": body = machine ? <MachineDetail machine={machine} view={view} /> : null; break;
    case "runner": body = runner ? <RunnerDetail runner={runner} view={view} /> : null; break;
    case "ignored": body = <IgnoredDetail view={view} />; break;
  }

  return (
    <main className="flex min-w-0 flex-1 flex-col bg-bg">
      <header data-tauri-drag-region="deep" className="flex h-[52px] shrink-0 items-center gap-2 border-b border-line px-4 xl:px-6">
        <nav aria-label="Breadcrumb" className="min-w-0 flex-1">
          <ol className="flex min-w-0 items-center gap-1 text-[13px]">
            {crumbs.map((crumb, i) => {
              const target = i < crumbs.length - 1 ? crumb.selection : undefined;
              return <li key={crumb.selection ? selectionId(crumb.selection) : "current"} className="flex min-w-0 items-center gap-1">
                {i > 0 && <span className="shrink-0 text-ink-3"><Icon name="chevron" size={13} /></span>}
                {target
                  ? <button className="truncate text-ink-2 hover:text-ink" onClick={() => select(target)}>{crumb.label}</button>
                  : <span className="truncate font-medium" aria-current={i === crumbs.length - 1 ? "page" : undefined} title={crumb.label}>{crumb.label}</span>}
              </li>;
            })}
          </ol>
        </nav>
        <span className="hidden shrink-0 text-[12px] text-ink-2 tabular lg:inline" title={new Date(view.generatedAt).toLocaleString()}>
          {view.source === "demo" ? "Demo data" : <>Updated <TimeAgo iso={view.generatedAt} /></>}
        </span>
        <Button small variant="primary" disabled={!isLive(view) || checking} onClick={() => check.mutate()}>
          {checking ? "Checking…" : "Check now"}
        </Button>
      </header>
      {check.isError && <div className="border-b border-line px-6 py-2"><ErrorText error={check.error} /></div>}
      {view.source === "demo" && <DemoBanner />}
      <div ref={scrollRef} className="scroll min-h-0 flex-1">
        <div key={current} className="mx-auto w-full max-w-[1200px] px-6 py-8 xl:px-8">{body}</div>
      </div>
    </main>
  );
}

function DemoBanner() {
  const live = useMutation({ mutationFn: () => commands.setDemoMode(false) });
  return (
    <div className="flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-demo-border bg-demo-bg px-5 py-2 text-[12px] text-demo-text">
      <span className="font-semibold tracking-wide">DEMO DATA</span>
      <span className="min-w-0 flex-1 truncate">Fictional machines and GitHub data.{isTauri ? " Turn off with View ▸ Toggle Demo Data (⇧⌘D)." : " Open the desktop app for live data."}</span>
      {isTauri && <button className="shrink-0 rounded-md border border-demo-border px-2 py-1 font-medium hover:bg-hover disabled:opacity-50" disabled={live.isPending} onClick={() => live.mutate()}>Show live data</button>}
      {live.isError && <ErrorText error={live.error} className="w-full" />}
    </div>
  );
}
