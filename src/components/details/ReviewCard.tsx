import { useMutation } from "@tanstack/react-query";
import { useEffect, useRef, useState } from "react";
import type { ReviewView } from "../../bindings/ReviewView";
import { requestTitle } from "../../lib/desired";
import { commands } from "../../lib/tauri";
import { Button, Callout, Card, ErrorText, Section } from "../ui";
import { Icon } from "../icons";
import { PlanItems, type Change } from "./changes";

/** The review (or its preparation) for a change opened with `useChange`. */
export function ChangePanel({ change, busy }: { change: Change; busy: boolean }) {
  if (change.preparing) {
    return <Card className="mt-4 px-4 py-3 text-[12.5px] text-ink-2">Reading the machine to prepare the review…</Card>;
  }
  if (change.error) return <Callout tone="bad" title="Couldn't prepare the review">{change.error}</Callout>;
  if (!change.review) return null;
  return <ReviewCard review={change.review} busy={busy} onClose={change.close} />;
}

function ReviewCard({ review: r, busy, onClose }: { review: ReviewView; busy: boolean; onClose: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  const [servicePassword, setServicePassword] = useState("");
  const [sudoPassword, setSudoPassword] = useState("");
  const [phrase, setPhrase] = useState("");
  const run = useMutation({
    mutationFn: () =>
      commands.desiredExecute(
        r.id,
        { servicePassword: servicePassword || null, sudoPassword: sudoPassword || null },
        r.confirmPhrase ? phrase : null,
      ),
    onSuccess: onClose,
  });
  useEffect(() => {
    ref.current?.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }, [r.id]);
  const dangerous = r.request.kind === "retire" || r.request.kind === "decommission";
  const ready =
    r.ready &&
    !busy &&
    (!r.needsServicePassword || servicePassword.length > 0) &&
    (!r.confirmPhrase || phrase.trim() === r.confirmPhrase);

  return (
    <div ref={ref}>
      <Section title={`Review: ${r.title}`} icon={<Icon name="plan" />}>
        <Card className="px-4 py-3.5 text-[12.5px]">
          <div className="text-ink-2">
            Read from the machine just now. When you run it, the machine is read again and nothing happens if anything changed.
          </div>
          {r.items.length > 0 && (
            <div className="mt-3">
              <PlanItems items={r.items} />
            </div>
          )}
          {r.blockers.length > 0 && (
            <ul className="mt-3 space-y-1 text-bad">
              {r.blockers.map((b) => (
                <li key={b} className="flex items-start gap-1.5 selectable">
                  <span className="mt-[1px]">
                    <Icon name="alert" size={12} />
                  </span>
                  {b}
                </li>
              ))}
            </ul>
          )}

          {r.ready && (
            <div className="mt-3 space-y-2">
              {r.needsServicePassword && (
                <Field label={`Password for ${r.needsServicePassword}`} hint="Needed to register the Windows service. Used once, never stored.">
                  <input
                    type="password"
                    className="input w-[260px]"
                    autoComplete="off"
                    value={servicePassword}
                    onChange={(e) => setServicePassword(e.target.value)}
                  />
                </Field>
              )}
              {r.acceptsSudoPassword && (
                <Field label="Sudo password (optional)" hint="Only if this machine lacks non-interactive sudo. Used once, never stored.">
                  <input
                    type="password"
                    className="input w-[260px]"
                    autoComplete="off"
                    value={sudoPassword}
                    onChange={(e) => setSudoPassword(e.target.value)}
                  />
                </Field>
              )}
              {r.confirmPhrase && (
                <Field label={`Type “${r.confirmPhrase}” to confirm`} hint="Stops and deregisters the exact runner; its directory is kept.">
                  <input className="input mono w-[260px]" autoComplete="off" value={phrase} onChange={(e) => setPhrase(e.target.value)} />
                </Field>
              )}
            </div>
          )}

          <div className="mt-3 flex items-center gap-2">
            {run.isError && <ErrorText error={run.error} className="min-w-0 flex-1" />}
            <div className="ml-auto flex gap-2">
              <Button variant="ghost" onClick={onClose}>
                Cancel
              </Button>
              <Button variant={dangerous ? "danger" : "primary"} disabled={!ready || run.isPending} onClick={() => run.mutate()}>
                {run.isPending ? "Re-checking…" : requestTitle(r.request)}
              </Button>
            </div>
          </div>
        </Card>
      </Section>
    </div>
  );
}

function Field({ label, hint, children }: { label: string; hint: string; children: React.ReactNode }) {
  return (
    <label className="flex items-center gap-3">
      <span className="w-[200px] shrink-0 text-ink-2">{label}</span>
      {children}
      <span className="text-[11.5px] text-ink-3">{hint}</span>
    </label>
  );
}
