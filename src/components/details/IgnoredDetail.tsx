import { useMutation } from "@tanstack/react-query";
import type { DiscoveredPeerView } from "../../bindings/DiscoveredPeerView";
import type { FleetView } from "../../bindings/FleetView";
import { useNow } from "../../hooks";
import { timeAgo } from "../../lib/format";
import { commands, isTauri } from "../../lib/tauri";
import { Button, ErrorText, StatusDot } from "../ui";
import { List, PageHeader } from "./shared";

export function IgnoredDetail({ view }: { view: FleetView }) {
  return (
    <div>
      <PageHeader
        title="Ignored machines"
        subtitle="Tailnet machines you chose not to track. Restoring one puts it back under Other machines."
      />
      <div className="mt-5">
        <List>
          {view.ignored.map((d) => <IgnoredRow key={d.peer.nodeId} d={d} />)}
        </List>
      </div>
    </div>
  );
}

function IgnoredRow({ d }: { d: DiscoveredPeerView }) {
  const now = useNow();
  const restore = useMutation({ mutationFn: () => commands.unignorePeer(d.peer.nodeId) });
  return (
    <div className="border-b border-line px-3.5 py-2 last:border-b-0">
      <div className="flex min-h-[28px] items-center gap-3">
        <StatusDot tone={d.peer.online ? "ok" : "muted"} hollow />
        <div className="min-w-0 flex-1">
          <div className="truncate font-medium">{d.peer.hostname}</div>
          <div className="truncate text-[11.5px] text-ink-3">
            {[d.peer.os, d.ignoredAt && `ignored ${timeAgo(d.ignoredAt, now)}`, !d.peer.dnsName && "not currently on the tailnet"]
              .filter(Boolean)
              .join(" · ")}
          </div>
        </div>
        {isTauri && (
          <Button small disabled={restore.isPending} onClick={() => restore.mutate()}>
            Restore
          </Button>
        )}
      </div>
      {restore.isError && <ErrorText error={restore.error} className="mt-1" />}
    </div>
  );
}
