import { useState } from "react";
import type { FleetView } from "../../bindings/FleetView";
import { groupByStem } from "../../lib/desired";
import { Section, StatusDot } from "../ui";
import { Icon } from "../icons";
import { List, RunnerLine } from "./shared";

/** Runners GitHub lists that aren't on any machine, grouped by name. */
export function Unlinked({ view }: { view: FleetView }) {
  const [open, setOpen] = useState<string | null>(null);
  const groups = groupByStem(view.unmatchedRunners);
  return (
    <Section title="Not linked to a machine" count={view.unmatchedRunners.length}>
      <p className="mb-2 px-0.5 text-[12px] text-ink-2">
        GitHub lists these runners, but none is on a known machine: typically hosted runner pools, or machines not in the
        inventory.
      </p>
      <List>
        {groups.map((g) =>
          g.runners.length === 1 ? (
            <RunnerLine key={g.name} r={g.runners[0]} view={view} />
          ) : (
            <div key={g.name}>
              <button
                type="button"
                aria-expanded={open === g.name}
                className="flex h-[40px] w-full items-center gap-3 border-b border-line px-3.5 text-left last:border-b-0 hover:bg-hover"
                onClick={() => setOpen(open === g.name ? null : g.name)}
              >
                <StatusDot tone="muted" hollow />
                <span className="font-medium">{g.name}</span>
                <span className="text-[11.5px] text-ink-3">
                  {g.runners.length} runners · {g.online} online{g.busy ? ` · ${g.busy} working` : ""}
                </span>
                <span className="ml-auto text-ink-3" style={{ transform: open === g.name ? "rotate(90deg)" : undefined }}>
                  <Icon name="chevron" size={11} />
                </span>
              </button>
              {open === g.name && g.runners.map((r) => <RunnerLine key={r.key} r={r} view={view} />)}
            </div>
          ),
        )}
      </List>
    </Section>
  );
}
