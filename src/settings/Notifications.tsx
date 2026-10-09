import { useMutation } from "@tanstack/react-query";
import { Button, Card, ErrorText, Switch } from "../components/ui";
import { commands, isTauri } from "../lib/tauri";

export function Notifications({ enabled, demo }: { enabled: boolean; demo: boolean }) {
  const toggle = useMutation({
    mutationFn: () => commands.setDesktopNotifications(!enabled),
  });
  const test = useMutation({ mutationFn: () => commands.testDesktopNotification() });
  return <>
    <h2 className="mt-5 text-[13px] font-semibold">Monitoring and notifications</h2>
    <p className="mt-1 text-[12px] text-ink-2">
      Alerts appear in the dashboard after 3 minutes offline or disk space below 10% on two checks at least a minute apart.
      Disk alerts clear at 12% free. History is saved on this Mac for 30 days, up to 5,000 events, while the app is running.
    </p>
    <Card className="mt-3 px-4 py-3">
      <div className="flex items-center gap-3">
        <div className="min-w-0 flex-1">
          <p className="text-[12.5px] font-medium">Desktop notifications</p>
          <p className="mt-1 text-[12px] text-ink-2">Notify once when an alert starts and when it recovers. Pause a machine for one hour from its Monitoring section.</p>
        </div>
        <Switch checked={enabled} label="Desktop notifications" disabled={!isTauri || toggle.isPending} onChange={() => { test.reset(); toggle.mutate(); }} />
      </div>
      {toggle.isError && <ErrorText error={toggle.error} className="mt-2" />}
      <div className="mt-3 flex flex-wrap items-center gap-2">
        <Button small disabled={!isTauri || !enabled || test.isPending} onClick={() => test.mutate()}>{test.isPending ? "Requesting…" : "Test notification"}</Button>
        {test.isSuccess && <p role="status" className="text-[12px] text-ink-2">Test notification requested.</p>}
      </div>
      {test.isError && <ErrorText error={test.error} className="mt-2" />}
      <p className="mt-2 text-[12px] text-ink-3">Delivery depends on macOS notification settings and Focus. Development builds may appear as Terminal.</p>
      {demo && <p className="mt-2 text-[12px] text-ink-3">Demo events never send notifications. The test button sends a sample only when you click it.</p>}
    </Card>
  </>;
}
