import type { FleetServiceMode } from "../../bindings/FleetServiceMode";
import type { MachineView } from "../../bindings/MachineView";
import { useNow } from "../../hooks";
import { formatUptime, timeAgo } from "../../lib/format";
import { EmptyLine, KeyValuePanel, Label, Section, type KeyValueRow } from "../ui";
import { CopyValue } from "../CopyValue";
import { List } from "./shared";

/** How the inventory declares the machine, and what the tailnet knows about it. */
export function MachineFacts({ m, inFleet }: { m: MachineView; inFleet: boolean }) {
  const now = useNow();
  const h = m.desired?.host;
  const t = m.tailnet;

  const tailnet: KeyValueRow[] = [
    ["Status", t ? (t.online ? "Online" : `Offline${t.lastSeen ? `, last seen ${timeAgo(t.lastSeen, now)}` : ""}`) : "Not found"],
  ];
  if (t) {
    tailnet.push(
      ["Hostname", t.hostname, t.hostname],
      ["MagicDNS", t.dnsName ?? "—", t.dnsName ?? undefined],
      ["Addresses", t.ips.length ? <span className="flex flex-col items-start gap-1">{t.ips.map((ip) => <CopyValue key={ip} value={ip} label="IP address" truncate />)}</span> : "—"],
      ["Tailscale SSH", m.diagnostics?.checks.find((c) => c.id === "tailscale-ssh")?.value ?? (t.sshHostKeys.length ? "Advertised" : "Not advertised")],
    );
    if (t.tags.length) tailnet.push(["Tags", t.tags.join(", "), t.tags.join(", ")]);
  }
  if (m.health?.uptimeSecs != null) tailnet.push(["Uptime", formatUptime(m.health.uptimeSecs)]);

  const inventory: KeyValueRow[] = [];
  if (h) {
    inventory.push(
      ["Host", <span className="mono">{h.id}</span>, h.id],
      ["Platform", h.platform, h.platform],
      ["Runners as", serviceModeLabel(h.serviceMode)],
      ["SSH", `${h.sshTarget ?? `unset ${h.sshSetting}`}${h.sshTransport === "tailscale" ? " · Tailscale SSH" : ""}`, h.sshTarget ?? undefined],
      ["Root", <span className="mono">{h.root}</span>, h.root],
    );
    if (h.serviceAccount) inventory.push(["Account", h.serviceAccount, h.serviceAccount]);
    inventory.push(["Labels", <span className="flex flex-wrap gap-1">{h.labels.map((l) => <Label key={l}>{l}</Label>)}</span>]);
    if (!h.enabled) inventory.push(["Status", "Disabled"]);
  }

  return (
    <Section title="Details">
      <div className="grid grid-cols-1 gap-3 min-[1100px]:grid-cols-2">
        {h ? (
          <KeyValuePanel title="Inventory" rows={inventory} />
        ) : (
          <List>
            <EmptyLine>
              {inFleet
                ? "Not in inventory/fleet.toml, so the app can't install or reconcile runners here. Add a [[hosts]] entry (docs/FLEET.md ▸ Add a host)."
                : "Not in inventory/fleet.toml."}
            </EmptyLine>
          </List>
        )}
        <KeyValuePanel title="Tailnet" rows={tailnet} />
      </div>
    </Section>
  );
}

function serviceModeLabel(mode: FleetServiceMode): string {
  switch (mode) {
    case "launchagent":
      return "LaunchAgents (start at login)";
    case "systemd":
      return "systemd services";
    case "windows-service":
      return "Windows services";
    case "windows-logon":
      return "logon tasks (desktop session)";
  }
}
