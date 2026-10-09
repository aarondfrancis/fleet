import { useMutation, useQuery } from "@tanstack/react-query";
import type { AppConfig } from "../bindings/AppConfig";
import { Card, ErrorText, Pill, Switch } from "../components/ui";
import { CopyValue } from "../components/CopyValue";
import { commands, isTauri } from "../lib/tauri";
import { Notifications } from "./Notifications";

export function GeneralSettings({ config }: { config: AppConfig }) {
  const demo = useMutation({ mutationFn: () => commands.setDemoMode(!config.demoMode) });
  return (
    <>
      <h2 className="text-[13px] font-semibold">Demo data</h2>
      <Card className="mt-2 px-4 py-3">
        <div className="flex items-center gap-3">
          <div className="min-w-0 flex-1 text-[12.5px] text-ink-2">
            Show a fictional fleet instead of live data. Live polling pauses while it's on, and every screen is labeled.
          </div>
          <Switch checked={config.demoMode} label="Demo data" disabled={!isTauri || demo.isPending} onChange={() => demo.mutate()} />
        </div>
        {demo.isError && <ErrorText error={demo.error} className="mt-2" />}
      </Card>

      <Notifications enabled={config.desktopNotifications} demo={config.demoMode} />

      <h2 className="mt-5 text-[13px] font-semibold">Local API for agents</h2>
      <p className="mt-1 text-[12px] text-ink-2">
        A read-only JSON API on this Mac only (127.0.0.1) that agents and scripts can use to inspect machines,
        runners, the queue and reviewed changes. It can't change anything and never returns tokens.
      </p>
      <LocalApi enabled={config.localApi} />

      <h2 className="mt-5 text-[13px] font-semibold">How access works</h2>
      <ul className="mt-2 list-disc space-y-1 pl-5 text-[12px] text-ink-2">
        <li>Machines are found with the Tailscale CLI on this Mac. No Tailscale API key is used.</li>
        <li>Health and runner configuration are read over SSH. No monitoring software needs to be installed on the machines.</li>
        <li>Busy/idle state and current jobs come from GitHub.</li>
        <li>Monitoring is read-only. Runner changes require a reviewed plan and use your existing SSH access.</li>
      </ul>
    </>
  );
}

function LocalApi({ enabled }: { enabled: boolean }) {
  const { data: status } = useQuery({ queryKey: ["local-api"], queryFn: commands.getLocalApiStatus });
  const toggle = useMutation({
    mutationFn: () => commands.setLocalApi(!enabled),
  });
  const pendingRestart = status && status.enabled !== enabled;
  return (
    <Card className="mt-2 px-4 py-3 text-[12.5px]">
      <div className="flex items-center gap-2">
        {status?.url ? (
          <Pill tone="ok">Listening</Pill>
        ) : status?.error ? (
          <Pill tone="bad">Not running</Pill>
        ) : (
          <Pill tone="muted">Off</Pill>
        )}
        <span className="mono min-w-0 flex-1 truncate text-ink-2 selectable">{status?.url ? <CopyValue value={status.url} label="API URL" truncate /> : status?.error ?? ""}</span>
        <Switch checked={enabled} label="Local API" disabled={!isTauri || toggle.isPending} onChange={() => toggle.mutate()} />
      </div>
      {toggle.isError && <ErrorText error={toggle.error} className="mt-1.5" />}
      {pendingRestart && <div className="mt-1.5 text-[11.5px] text-warn">Takes effect the next time the app starts.</div>}
      {status?.url && (
        <div className="mono mt-2 flex flex-col items-start gap-1 overflow-hidden rounded-[6px] bg-sunken px-2 py-1.5 text-[11.5px] text-ink">
          {["/", "/v1/summary", "/v1/runners"].map((path) => <CopyValue key={path} value={`curl -s ${status.url}${path}`} label="Command" truncate />)}
        </div>
      )}
      {status?.discoveryFile && (
        <div className="mt-1.5 truncate text-[11.5px] text-ink-3 selectable">Discovery file: <CopyValue value={status.discoveryFile} label="Discovery path" truncate /></div>
      )}
    </Card>
  );
}
