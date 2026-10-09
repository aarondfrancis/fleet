import type { ReactNode } from "react";
import type { MachineView } from "../../bindings/MachineView";
import { useNow } from "../../hooks";
import { changes } from "../../lib/desired";
import { plural, timeAgo } from "../../lib/format";
import { hasTailscaleSsh, type Tone } from "../../lib/status";
import { Stat, StatStrip } from "../ui";
import { CopyValue } from "../CopyValue";

interface Fact {
  tone: Tone;
  value: string;
  sub: ReactNode;
  hollow?: boolean;
}

/** Tailnet reachability, the last SSH read, and inventory drift. */
export function MachineGlance({ m }: { m: MachineView }) {
  const now = useNow();
  const t = m.tailnet;
  const sshTarget = m.desired?.host.sshTarget;
  const tailnet: Fact = !t
    ? { tone: "bad", value: "Not found", sub: sshTarget ? <CopyValue value={sshTarget} label="SSH target" truncate /> : "no tailnet match" }
    : t.online
      ? { tone: "ok", value: "Online", sub: hasTailscaleSsh(m) ? "Tailscale SSH on" : "SSH over the tailnet" }
      : { tone: "muted", value: "Offline", sub: t.lastSeen ? `last seen ${timeAgo(t.lastSeen, now)}` : "not seen recently" };

  const c = m.connection;
  const ssh: Fact =
    c.kind === "connected"
      ? { tone: "ok", value: "Connected", sub: `read ${timeAgo(c.checkedAt, now)}` }
      : c.kind === "stale"
        ? { tone: "warn", value: "Stale", sub: `last read ${timeAgo(c.checkedAt, now)}`, hollow: true }
        : c.kind === "error"
          ? { tone: "bad", value: "Couldn't connect", sub: <CopyValue value={c.detail} label="SSH error" truncate /> }
          : c.kind === "offline"
            ? { tone: "muted", value: "Offline", sub: "machine is offline", hollow: true }
            : c.kind === "disabled"
              ? { tone: "muted", value: "Disabled", sub: "disabled in the inventory", hollow: true }
              : { tone: "muted", value: "Checking…", sub: "read automatically every minute", hollow: true };

  const d = m.desired;
  const pending = d ? changes(d.plan).filter((i) => !i.blocked).length : 0;
  const inventory: Fact = !d
    ? { tone: "muted", value: "Not in inventory", sub: "no desired state", hollow: true }
    : d.error
      ? { tone: "bad", value: "Couldn't check", sub: "the machine didn't answer" }
      : !d.checkedAt
        ? { tone: "muted", value: "Not checked", sub: "Check now compares it with GitHub", hollow: true }
        : pending > 0
          ? { tone: "busy", value: plural(pending, "change"), sub: `checked ${timeAgo(d.checkedAt, now)}` }
          : changes(d.plan).length > 0
            ? { tone: "warn", value: "Needs a look", sub: `checked ${timeAgo(d.checkedAt, now)}` }
            : { tone: "ok", value: "In sync", sub: `checked ${timeAgo(d.checkedAt, now)}` };

  return (
    <StatStrip cols={3}>
      <Stat label="Tailnet" {...tailnet} />
      <Stat label="SSH" {...ssh} />
      <Stat label="Inventory" {...inventory} />
    </StatStrip>
  );
}
