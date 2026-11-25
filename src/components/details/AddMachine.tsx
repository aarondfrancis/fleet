import { useState } from "react";
import type { FleetView } from "../../bindings/FleetView";
import { CopyValue } from "../CopyValue";
import { Icon } from "../icons";
import { Card } from "../ui";
import { List, PageHeader } from "./shared";

/** Tailnet devices outside the inventory, and the entry that adds each one. Nothing here contacts a device. */
export function AddMachine({ view }: { view: FleetView }) {
  const [open, setOpen] = useState<string | null>(view.tailnetDevices[0]?.nodeId ?? null);
  return (
    <div>
      <PageHeader
        icon={<Icon name="plus" size={18} />}
        title="Add machine"
        subtitle="Tailnet devices that aren't in the inventory. The app doesn't contact a device until you add it there."
      />
      <div className="mt-5">
        <List>
          {view.tailnetDevices.map((d) => (
            <div key={d.nodeId} className="border-b border-line last:border-b-0">
              <button
                type="button"
                aria-expanded={open === d.nodeId}
                className="flex h-[40px] w-full items-center gap-3 px-3.5 text-left hover:bg-hover"
                onClick={() => setOpen(open === d.nodeId ? null : d.nodeId)}
              >
                <span className="text-ink-2"><Icon name="machine" /></span>
                <span className="min-w-0 flex-1 truncate font-medium">{d.name}</span>
                <span className="text-ink-3" style={{ transform: open === d.nodeId ? "rotate(90deg)" : undefined }}>
                  <Icon name="chevron" size={11} />
                </span>
              </button>
              {open === d.nodeId && (
                <div className="px-3.5 pb-3.5">
                  <p className="text-[12.5px] text-ink-2">
                    Add this entry to <span className="font-mono text-ink selectable">{view.plan.inventoryPath}</span>.
                    Check the platform and SSH target, then add runners when it should take jobs.
                  </p>
                  <Card className="mt-2 bg-sunken px-3 py-2.5">
                    <pre className="font-mono text-[12px] leading-relaxed whitespace-pre-wrap text-ink selectable">{d.inventoryEntry}</pre>
                  </Card>
                  <CopyValue value={d.inventoryEntry} label="Inventory entry" className="mt-2 text-[12.5px] font-medium text-accent">
                    Copy entry
                  </CopyValue>
                </div>
              )}
            </div>
          ))}
        </List>
      </div>
    </div>
  );
}
