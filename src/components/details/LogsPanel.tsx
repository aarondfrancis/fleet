import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import { commands } from "../../lib/tauri";
import { Button, Card, ErrorText, Section } from "../ui";
import { CopyValue } from "../CopyValue";

/** Read-only tail of one runner's diagnostic log. */
export function LogsPanel({ runner, installDir }: { runner: string; installDir?: string }) {
  const [open, setOpen] = useState(false);
  const separator = installDir?.includes("\\") ? "\\" : "/";
  const logPath = `${installDir ?? runner}${separator}_diag${separator}Runner_*.log`;
  const logs = useMutation({ mutationFn: () => commands.desiredLogs(runner, 200), onSuccess: () => setOpen(true) });
  return (
    <Section
      title="Diagnostic log"
      action={
        open ? (
          <div className="flex items-center gap-3">
            {logs.data && <CopyValue value={logs.data} label="Log" className="text-[12.5px] text-ink-2">Copy log</CopyValue>}
            <Button small variant="ghost" onClick={() => setOpen(false)}>Hide</Button>
          </div>
        ) : (
          <Button small variant="ghost" disabled={logs.isPending} onClick={() => logs.mutate()}>
            {logs.isPending ? "Reading…" : "Show last 200 lines"}
          </Button>
        )
      }
    >
      {logs.isError && <ErrorText error={logs.error} className="mb-2" />}
      {open && logs.data !== undefined && (
        <Card className="max-h-[360px] overflow-auto bg-sunken p-4">
          <pre className="font-mono text-[12px] leading-relaxed whitespace-pre-wrap text-ink selectable">{logs.data || "(empty)"}</pre>
        </Card>
      )}
      {!open && <Card className="px-4 py-3 font-mono text-[12px] text-ink-2"><CopyValue value={logPath} label="Log path" truncate /></Card>}
    </Section>
  );
}
