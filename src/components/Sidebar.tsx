import { useEffect, useRef } from "react";
import type { FleetView } from "../bindings/FleetView";
import type { MachineView } from "../bindings/MachineView";
import type { RunnerView } from "../bindings/RunnerView";
import { formatClock } from "../lib/format";
import { allMachines, selectionId, type Selection } from "../lib/nav";
import { machineNeedsChanges, runnerNeedsChanges } from "../lib/desired";
import { busySince, hasWarning, machineDot, machineStatus, machineSummary, machineTooltip, runnerStatus, tailnetState } from "../lib/status";
import { emitEvent } from "../lib/tauri";
import { useUi } from "../store";
import { Icon } from "./icons";
import { Elapsed, EmptyLine, StatusDot, TimeAgo } from "./ui";

/** `filtered` is `view` narrowed by the search query. */
export function Sidebar({ view, filtered, selection }: { view: FleetView; filtered: FleetView; selection: Selection }) {
  const collapsed = useUi((s) => s.collapsed);
  const open = useUi((s) => s.open);
  const setOpen = useUi((s) => s.setOpen);
  const query = useUi((s) => s.query);
  const setQuery = useUi((s) => s.setQuery);
  const scrollRef = useRef<HTMLDivElement>(null);
  const selectedId = selectionId(selection);
  const searching = !!query.trim();
  const orgs = view.orgs.map((o) => o.login).join(", ") || view.plan.organization?.name;
  const isSelected = (sel: Selection) => selectionId(sel) === selectedId;

  useEffect(() => {
    scrollRef.current?.querySelector(`[data-nav="${CSS.escape(selectedId)}"]`)?.scrollIntoView({ block: "nearest" });
  }, [selectedId]);

  return (
    <aside aria-label="Fleet navigation" className="flex h-full w-[256px] shrink-0 flex-col border-r border-line bg-sidebar">
      <div data-tauri-drag-region="deep" className="flex h-[52px] shrink-0 items-center gap-2 border-b border-line pr-3 pl-[92px]">
        <span className="flex size-6 shrink-0 items-center justify-center rounded-md bg-ink text-bg"><Icon name="server" /></span>
        <div className="min-w-0 leading-tight">
          <div className="text-[13px] font-semibold">Runner Dashboard</div>
          <div className="mt-0.5 truncate text-[11px] text-ink-2" title={orgs}>{orgs ? `${orgs} · self-hosted` : "Self-hosted runners"}</div>
        </div>
      </div>
      <nav className="space-y-0.5 p-2" aria-label="Overview">
        <Row sel={{ kind: "overview" }} selected={isSelected({ kind: "overview" })} icon={<Icon name="grid" size={16} />} label="Fleet overview" trailing={allMachines(view).length} />
        <Row sel={{ kind: "queue" }} selected={isSelected({ kind: "queue" })} icon={<Icon name="queue" size={16} />} label="Queued jobs" trailing={view.queue.jobs.length} />
        <Row sel={{ kind: "activity" }} selected={isSelected({ kind: "activity" })} icon={<Icon name="activity" size={16} />} label="Activity" />
      </nav>
      <div className="px-2 pb-2">
        <label className="relative block">
          <span className="sr-only">Filter</span>
          <span className="pointer-events-none absolute top-1/2 left-2 -translate-y-1/2 text-ink-2"><Icon name="search" /></span>
          <input
            className="input !pl-7 !pr-7 text-[13px]"
            placeholder="Filter"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => { if (e.key === "Escape") { setQuery(""); e.currentTarget.blur(); } }}
          />
        </label>
        {query && <button aria-label="Clear filter" className="float-right relative -mt-7 flex h-7 w-7 items-center justify-center text-ink-2 hover:text-ink" onClick={() => setQuery("")}><Icon name="x" size={12} /></button>}
      </div>
      <nav ref={scrollRef} className="scroll min-h-0 flex-1 px-2 pb-2" aria-label="Machines and runners">
        <Heading title="Machines" count={filtered.machines.length} />
        {view.machines.length === 0 && !searching && <EmptyLine>{view.tailnet.available ? "Add runner machines to the inventory to see them here." : tailnetState(view.tailnet).text}</EmptyLine>}
        {filtered.machines.map((m) => <MachineRows key={m.id} m={m} open={searching || !collapsed.has(m.id)} searching={searching} isSelected={isSelected} />)}
        {filtered.unmatchedRunners.length > 0 && <>
          <Heading title="Not linked to a machine" count={filtered.unmatchedRunners.length} open={searching || open.unlinked} onToggle={() => setOpen("unlinked", !open.unlinked)} />
          {(searching || open.unlinked) && filtered.unmatchedRunners.map((r) => <RunnerRow key={r.key} r={r} isSelected={isSelected} />)}
        </>}
        {filtered.otherMachines.length > 0 && <>
          <Heading title="Other machines" count={filtered.otherMachines.length} open={searching || open.other} onToggle={() => setOpen("other", !open.other)} />
          {(searching || open.other) && filtered.otherMachines.map((m) => {
            const status = machineStatus(m.connection);
            const sel: Selection = { kind: "machine", id: m.id };
            return <Row key={m.id} sel={sel} selected={isSelected(sel)} icon={<StatusDot tone={status.tone} hollow={status.hollow} />} label={m.displayName} title={machineTooltip(m)} trailing={status.label} />;
          })}
        </>}
        {filtered.ignored.length > 0 && <Row sel={{ kind: "ignored" }} selected={isSelected({ kind: "ignored" })} icon={<Icon name="eye-off" />} label="Ignored" trailing={filtered.ignored.length} />}
        {searching && !filtered.machines.length && !filtered.otherMachines.length && !filtered.unmatchedRunners.length && !filtered.ignored.length && <EmptyLine>No machines or runners match “{query}”.</EmptyLine>}
      </nav>
      <Footer view={view} />
    </aside>
  );
}

function MachineRows({ m, open, searching, isSelected }: { m: MachineView; open: boolean; searching: boolean; isSelected: (sel: Selection) => boolean }) {
  const toggle = useUi((s) => s.toggleCollapsed);
  const dot = machineDot(m);
  const sel: Selection = { kind: "machine", id: m.id };
  return (
    <div>
      <Row
        sel={sel}
        selected={isSelected(sel)}
        icon={<StatusDot tone={dot.tone} hollow={dot.hollow} />}
        label={m.displayName}
        title={machineTooltip(m)}
        trailing={machineSummary(m)}
        warn={machineNeedsChanges(m)}
        disclosure={m.runners.length ? { open, onToggle: () => { if (!searching) toggle(m.id); } } : undefined}
      />
      {open && m.runners.map((r) => <RunnerRow key={r.key} r={r} isSelected={isSelected} nested />)}
    </div>
  );
}

function RunnerRow({ r, isSelected, nested }: { r: RunnerView; isSelected: (sel: Selection) => boolean; nested?: boolean }) {
  const status = runnerStatus(r);
  const since = busySince(r);
  const sel: Selection = { kind: "runner", key: r.key };
  return (
    <div className={nested ? "tree-child" : undefined}>
      <Row
        sel={sel}
        selected={isSelected(sel)}
        label={r.name}
        indent={nested}
        warn={hasWarning(r) || runnerNeedsChanges(r)}
        title={`${r.name}: ${status.label}${status.hollow ? " (last known state)" : ""}`}
        trailing={<span className={`flex items-center gap-1.5 ${status.tone === "busy" ? "text-accent" : ""}`}>
          {since ? <span title={`Working since ${formatClock(since)}`}><Elapsed since={since} /></span> : status.tone === "busy" ? status.label : <span className="sr-only">{status.label}</span>}
          <StatusDot tone={status.tone} hollow={status.hollow} size={6} />
        </span>}
      />
    </div>
  );
}

function Row({ sel, selected, icon, label, trailing, indent, warn, disclosure, title }: {
  sel: Selection; selected: boolean; icon?: React.ReactNode; label: string; trailing?: React.ReactNode; indent?: boolean; warn?: boolean;
  disclosure?: { open: boolean; onToggle: () => void }; title?: string;
}) {
  const select = useUi((s) => s.select);
  return (
    <div className={`group flex rounded-md ${indent ? "ml-5 h-6" : "h-7"} ${selected ? "bg-selected" : "hover:bg-hover"}`}>
      {disclosure && <button type="button" aria-label={`${disclosure.open ? "Collapse" : "Expand"} ${label}`} aria-expanded={disclosure.open} onClick={disclosure.onToggle} className="flex w-6 shrink-0 items-center justify-center text-ink-2">
        <span style={{ transform: disclosure.open ? "rotate(90deg)" : undefined, transition: "transform 120ms" }}><Icon name="chevron" size={13} /></span>
      </button>}
      <button data-nav={selectionId(sel)} type="button" title={title} aria-current={selected ? "page" : undefined} onClick={() => select(sel)} className={`flex min-w-0 flex-1 items-center gap-2 pr-2 text-left ${disclosure ? "" : "pl-2"} ${selected ? "font-medium" : ""}`}>
        {icon && <span className="flex size-4 shrink-0 items-center justify-center text-ink-2">{icon}</span>}
        <span className={`min-w-0 flex-1 truncate ${sel.kind === "runner" ? "text-[12px]" : "text-[13px]"}`}>{label}</span>
        {warn && <span className="text-warn" title="Needs attention"><Icon name="alert" size={12} /></span>}
        {trailing != null && <span className="shrink-0 text-[11px] text-ink-2 tabular">{trailing}</span>}
      </button>
    </div>
  );
}

function Heading({ title, count, open, onToggle }: { title: string; count: number; open?: boolean; onToggle?: () => void }) {
  const content = <><span className="truncate">{title}</span><span className="ml-auto tabular">{count}</span>{onToggle && <span style={{ transform: open ? "rotate(90deg)" : undefined }}><Icon name="chevron" size={12} /></span>}</>;
  const className = "mt-2 flex h-7 w-full items-center gap-2 px-2 text-left text-[11px] font-medium tracking-wide text-ink-2 uppercase";
  return onToggle ? <button type="button" onClick={onToggle} aria-expanded={open} className={className}>{content}</button> : <div className={className}>{content}</div>;
}

function Footer({ view }: { view: FleetView }) {
  const checked = view.orgs.map((o) => o.runnersAsOf).filter((t): t is string => !!t).sort()[0];
  const problem = view.orgs.some((o) => o.error) || !view.tailnet.available || !view.orgs.length;
  const text = view.source === "demo" ? "Demo · fictional machines"
    : !view.tailnet.available ? tailnetState(view.tailnet).text
    : checked ? <>GitHub checked <TimeAgo iso={checked} /></>
    : view.orgs.length ? "Waiting for GitHub" : "No GitHub org · using last check";
  return (
    <div className="flex h-9 shrink-0 items-center gap-2 border-t border-line px-3 text-[11px] text-ink-2">
      <StatusDot tone={problem ? "warn" : "ok"} size={6} />
      <span className="min-w-0 flex-1 truncate" title={typeof text === "string" ? text : undefined}>{text}</span>
      <button type="button" aria-label="Settings" title="Settings (⌘,)" onClick={() => emitEvent("open-settings")} className="flex size-6 shrink-0 items-center justify-center rounded-md hover:bg-hover"><Icon name="settings" /></button>
    </div>
  );
}
