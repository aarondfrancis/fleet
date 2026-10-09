import { useConfig, useFleet } from "../hooks";
import { GeneralSettings } from "./GeneralSettings";
import { GitHubSettings } from "./GitHubSettings";
import { MachinePreferences } from "./MachinePreferences";
import { Tabs } from "./Tabs";

export function Settings() {
  const { data: config } = useConfig();
  const { data: view } = useFleet();
  if (!config || !view) return null;
  return (
    <Tabs
      label="Settings sections"
      panels={{
        Machines: <>
          <h2 className="text-[13px] font-semibold">Machines</h2>
          <p className="mt-1 text-[12px] text-ink-2">
            Personal names and notes, saved only on this Mac.
          </p>
          <MachinePreferences view={view} />
        </>,
        GitHub: <GitHubSettings orgs={config.orgs} status={view.orgs} />,
        General: <GeneralSettings config={config} />,
      }}
    />
  );
}
