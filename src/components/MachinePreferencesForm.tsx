import { useMutation, useQueryClient } from "@tanstack/react-query";
import { useId, useState } from "react";
import type { MachineView } from "../bindings/MachineView";
import { commands } from "../lib/tauri";
import { Button, ErrorText } from "./ui";

/** The name a machine shows without a personal one: its inventory host or tailnet hostname. */
export function defaultMachineName(m: MachineView): string {
  return m.desired?.host.id ?? m.tailnet?.hostname ?? m.host;
}

export function MachinePreferencesForm({ machine: m, close }: { machine: MachineView; close: () => void }) {
  const originalName = defaultMachineName(m);
  const qc = useQueryClient();
  const formId = useId();
  const initialName = m.displayName === originalName ? "" : m.displayName;
  const [name, setName] = useState(initialName);
  const [description, setDescription] = useState(m.description ?? "");
  const save = useMutation({
    mutationFn: (prefs: { displayName: string; description: string }) => commands.setMachinePreferences(m.id, prefs.displayName, prefs.description),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["fleet"] });
      close();
    },
  });
  const busy = save.isPending;
  return (
    <form className="mt-3 grid gap-x-4 gap-y-3 sm:grid-cols-2" onSubmit={(e) => { e.preventDefault(); save.mutate({ displayName: name, description }); }} onKeyDown={(e) => {
      if (e.key === "Escape" && !busy) { e.preventDefault(); e.stopPropagation(); close(); }
    }}>
      <div className="min-w-0">
        <label htmlFor={`${formId}-name`} className="block text-[12px] text-ink-2">Display name</label>
        <input id={`${formId}-name`} className="input mt-1 w-full" value={name} onChange={(e) => setName(e.target.value)} placeholder={originalName} maxLength={120} disabled={busy} autoFocus />
        <div className="mt-1.5 break-words text-[11.5px] text-ink-3">Leave blank to use {originalName}.</div>
      </div>
      <div className="min-w-0">
        <label htmlFor={`${formId}-description`} className="block text-[12px] text-ink-2">Description</label>
        <textarea id={`${formId}-description`} className="input mt-1 h-auto min-h-[76px] w-full resize-y py-2" rows={3} value={description} onChange={(e) => setDescription(e.target.value)} placeholder="Office closet, top shelf. Dedicated to macOS GUI tests." maxLength={2000} disabled={busy} />
      </div>
      {save.isError && <ErrorText error={save.error} className="sm:col-span-2" />}
      <div className="flex items-center gap-2 pb-1 sm:col-span-2">
        <Button type="submit" variant="primary" disabled={busy}>{busy ? "Saving…" : "Save"}</Button>
        <Button type="button" onClick={close} disabled={busy}>Cancel</Button>
        {(initialName || m.description) && <span className="ml-auto"><Button type="button" variant="ghost" onClick={() => save.mutate({ displayName: "", description: "" })} disabled={busy}>Reset</Button></span>}
      </div>
    </form>
  );
}
