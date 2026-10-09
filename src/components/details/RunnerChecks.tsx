import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import type { FleetCheckStatus } from "../../bindings/FleetCheckStatus";
import type { FleetDoctorCheck } from "../../bindings/FleetDoctorCheck";
import type { Tone } from "../../lib/status";
import { commands } from "../../lib/tauri";
import { Button, Card, EmptyLine, ErrorText, KeyValue, Section, StatusDot, TimeAgo } from "../ui";
import { List } from "./shared";

const CHECK_TONE: Record<FleetCheckStatus, Tone> = { pass: "ok", warn: "warn", fail: "bad" };

/** Runner checks (Runner Fleet's doctor) for one inventory host, or for the fleet with `host` null. */
export function RunnerChecks({ host, checks, checkedAt, running, live, notRun }: {
  host: string | null;
  checks: FleetDoctorCheck[];
  checkedAt: string | null;
  running: boolean;
  live: boolean;
  /** What the checks look at, shown until they first run. */
  notRun: string;
}) {
  const doctor = useMutation({ mutationFn: () => commands.desiredDoctor(host) });
  return (
    <Section
      title="Runner checks"
      action={
        <Button small variant="ghost" disabled={!live || running || doctor.isPending} onClick={() => doctor.mutate()}>
          {running ? "Running checks…" : "Run checks"}
        </Button>
      }
    >
      {doctor.isError && <ErrorText error={doctor.error} className="mb-2" />}
      {checkedAt ? <ChecksList checks={checks} checkedAt={checkedAt} /> : <List><EmptyLine>{notRun}</EmptyLine></List>}
    </Section>
  );
}

function ChecksList({ checks, checkedAt }: { checks: FleetDoctorCheck[]; checkedAt: string }) {
  const [all, setAll] = useState(true);
  const shown = all ? checks : checks.filter((c) => c.status !== "pass");
  const fails = checks.filter((c) => c.status === "fail").length;
  const warns = checks.filter((c) => c.status === "warn").length;
  return (
    <Card className="px-3.5 py-2.5 text-[12.5px]">
      <div className="mb-1.5 flex items-center text-ink-3">
        <span>
          <TimeAgo iso={checkedAt} /> · {fails} failing · {warns} warnings · read-only
        </span>
        {checks.length > 0 && (
          <button className="ml-auto text-[11.5px] text-accent" onClick={() => setAll(!all)}>
            {all ? "Only problems" : `Show all ${checks.length}`}
          </button>
        )}
      </div>
      {checks.length > 0 && shown.length === 0 && <div className="text-ink-2">Every check passed.</div>}
      <KeyValue
        rows={shown.map((c) => [
          c.check,
          <span key={c.check} className="flex items-start gap-1.5" title={c.detail}>
            <StatusDot tone={CHECK_TONE[c.status]} size={6} />
            <span className="truncate">{c.detail}</span>
          </span>,
        ])}
      />
    </Card>
  );
}
