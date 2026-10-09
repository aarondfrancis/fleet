import { useState } from "react";
import type { FleetView } from "../bindings/FleetView";
import type { MachineView } from "../bindings/MachineView";
import { Button, Card, EmptyLine } from "../components/ui";
import { allMachines } from "../lib/nav";
import { isLive } from "../lib/tauri";
import { defaultMachineName, MachinePreferencesForm } from "../components/MachinePreferencesForm";
import { CopyValue } from "../components/CopyValue";

export function MachinePreferences({ view }: { view: FleetView }) {
  const machines = allMachines(view);
  const editable = isLive(view);
  return (
    <div className="mt-3">
      {view.source === "demo" && <p className="mb-3 text-[12px] text-ink-3">Turn off demo data in General to label your own machines.</p>}
      <Card className="divide-y divide-line">
        {machines.length === 0 && <EmptyLine>Machines appear here when they're found on your tailnet or in the inventory.</EmptyLine>}
        {machines.map((machine) => <MachineRow key={machine.id} machine={machine} editable={editable} />)}
      </Card>
    </div>
  );
}

function MachineRow({ machine: m, editable }: { machine: MachineView; editable: boolean }) {
  const [editing, setEditing] = useState(false);
  const originalName = defaultMachineName(m);
  return (
    <div className="px-3 py-2">
      <div className="grid min-h-[36px] grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] items-center gap-4">
        <div className="min-w-0">
          <div className="truncate font-medium" title={m.displayName}>{m.displayName}</div>
          <div className="mt-0.5 truncate text-[11.5px] text-ink-3" title={m.host}>
            {m.displayName !== originalName ? `${originalName} · ` : ""}<CopyValue value={m.host} label="Address" truncate />
          </div>
        </div>
        <p className="line-clamp-2 min-w-0 whitespace-pre-wrap break-words text-[12px] text-ink-2" title={m.description ?? undefined}>{m.description}</p>
        <div className="min-w-[50px]">{!editing && <Button small onClick={() => setEditing(true)} disabled={!editable} aria-label={`Edit ${m.displayName}`}>Edit…</Button>}</div>
      </div>
      {editing && (
        <MachinePreferencesForm machine={m} close={() => setEditing(false)} />
      )}
    </div>
  );
}
