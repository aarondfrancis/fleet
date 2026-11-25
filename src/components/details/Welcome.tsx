import { useMutation } from "@tanstack/react-query";
import type { FleetView } from "../../bindings/FleetView";
import { commands, emitEvent, isTauri } from "../../lib/tauri";
import { Button, Card, ErrorText } from "../ui";
import { InventoryProblem } from "./shared";

/** First run: no organization and no machines yet. */
export function Welcome({ view }: { view: FleetView }) {
  const demo = useMutation({ mutationFn: () => commands.setDemoMode(true) });
  return (
    <div className="mx-auto mt-10 max-w-[520px] text-center">
      <h1 className="text-[22px] font-semibold">Runner Dashboard</h1>
      <p className="mt-2 text-ink-2">
        Watch your self-hosted GitHub runners: which machines are up, which runners are connected, and what they're
        working on.
      </p>
      <div className="text-left"><InventoryProblem view={view} /></div>
      <Card className="mt-6 px-5 py-4 text-left">
        <ol className="space-y-3 text-[12.5px]">
          <li>
            <span className="font-medium">1. Add a GitHub organization.</span>{" "}
            <span className="text-ink-2">It uses your GitHub CLI (gh) login; nothing is copied.</span>
          </li>
          <li>
            <span className="font-medium">2. Describe your machines in inventory/fleet.toml.</span>{" "}
            <span className="text-ink-2">
              Only machines in the inventory are shown and checked. Add machine lists your tailnet devices and the entry
              for each one.
            </span>
          </li>
          <li>
            <span className="font-medium">3. Connect over SSH.</span>{" "}
            <span className="text-ink-2">The app reads health and runner details every minute using your SSH access.</span>
          </li>
        </ol>
      </Card>
      <div className="mt-5 flex justify-center gap-2">
        <Button variant="primary" onClick={() => emitEvent("open-settings")}>
          Add organization
        </Button>
        {isTauri && <Button disabled={demo.isPending} onClick={() => demo.mutate()}>Explore demo data</Button>}
      </div>
      {demo.isError && <ErrorText error={demo.error} className="mt-3" />}
    </div>
  );
}
