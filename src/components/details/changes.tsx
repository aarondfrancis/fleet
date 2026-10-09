import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import type { ChangeRequest } from "../../bindings/ChangeRequest";
import type { FleetPlanItem } from "../../bindings/FleetPlanItem";
import type { OperationStatus } from "../../bindings/OperationStatus";
import type { OperationView } from "../../bindings/OperationView";
import type { ReviewView } from "../../bindings/ReviewView";
import { actionLabel, itemTone } from "../../lib/desired";
import type { Tone } from "../../lib/status";
import { commands, errorText } from "../../lib/tauri";
import { Button, Card, ErrorText, Pill, Section, StatusDot, TimeAgo } from "../ui";
import { Icon } from "../icons";

/** Opens a review for a change: reads the host again and shows what would happen. */
export function useChange() {
  const [review, setReview] = useState<ReviewView | null>(null);
  const prepare = useMutation({ mutationFn: commands.desiredReview, onSuccess: setReview });
  return {
    review,
    preparing: prepare.isPending,
    error: prepare.isError ? errorText(prepare.error) : null,
    open: (request: ChangeRequest) => {
      setReview(null);
      prepare.mutate(request);
    },
    close: () => {
      setReview(null);
      prepare.reset();
    },
  };
}

export type Change = ReturnType<typeof useChange>;

/** Plan items: what happens to which runner, with the reason underneath. */
export function PlanItems({ items, action }: { items: FleetPlanItem[]; action?: (i: FleetPlanItem) => React.ReactNode }) {
  return (
    <div className="divide-y divide-line">
      {items.map((i) => (
        <PlanItemRow key={`${i.runner}:${i.action}`} item={i} action={action?.(i)} />
      ))}
    </div>
  );
}

function PlanItemRow({ item: i, action }: { item: FleetPlanItem; action?: React.ReactNode }) {
  return (
    <div className="flex items-center gap-3 py-2 text-[12.5px] first:pt-0 last:pb-0">
      <StatusDot tone={itemTone(i)} size={7} hollow={i.blocked} />
      <div className="min-w-0 flex-1">
        <div>
          <span className="font-medium">{actionLabel(i.action)}</span>
          <span className="text-ink-2"> · {i.runner}</span>
          {i.blocked && <span className="text-warn"> · won't change</span>}
        </div>
        <div className="truncate text-[11.5px] text-ink-3 selectable" title={i.reason}>
          {i.reason}
        </div>
      </div>
      {action}
    </div>
  );
}

const OPERATION_STATUS: Record<OperationStatus, { label: string; tone: Tone }> = {
  running: { label: "Running", tone: "busy" },
  succeeded: { label: "Done", tone: "ok" },
  failed: { label: "Failed", tone: "bad" },
};

/** The change that's running or last ran. */
export function OperationCard({ op }: { op: OperationView }) {
  const dismiss = useMutation({ mutationFn: commands.desiredDismissOperation });
  const status = OPERATION_STATUS[op.status];
  return (
    <Section title={op.title}>
      <Card className="px-4 py-3 text-[12.5px]">
        <div className="flex items-center gap-2">
          <Pill tone={status.tone}>{status.label}</Pill>
          <span className="text-ink-3">
            {op.finishedAt ? <>finished <TimeAgo iso={op.finishedAt} /></> : <>started <TimeAgo iso={op.startedAt} /></>}
          </span>
          {op.status !== "running" && (
            <div className="ml-auto">
              <Button small variant="ghost" disabled={dismiss.isPending} onClick={() => dismiss.mutate()}>
                Dismiss
              </Button>
            </div>
          )}
        </div>
        {op.messages.length > 0 && (
          <ul className="mt-2 space-y-0.5">
            {op.messages.map((m) => (
              <li key={m} className="flex items-start gap-1.5 selectable">
                <span className="mt-[1px] text-ok">
                  <Icon name="check" size={12} />
                </span>
                {m}
              </li>
            ))}
          </ul>
        )}
        {op.error && <div className="mt-2 text-bad selectable">{op.error}</div>}
        {dismiss.isError && <ErrorText error={dismiss.error} className="mt-2" />}
      </Card>
    </Section>
  );
}
