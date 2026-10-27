/**
 * Typed wrappers over Tauri commands and events.
 *
 * Outside Tauri (plain `pnpm dev` in a browser) everything is served from the
 * demo fixture so layouts can be iterated on; the view is marked `demo`.
 */
import type { AppConfig } from "../bindings/AppConfig";
import type { FleetView } from "../bindings/FleetView";
import type { FleetMachineEvent } from "../bindings/FleetMachineEvent";
import type { ChangeRequest } from "../bindings/ChangeRequest";
import type { ChangeSecrets } from "../bindings/ChangeSecrets";
import type { ReviewView } from "../bindings/ReviewView";
import type { GhStatus } from "../bindings/GhStatus";
import type { AccountSetup } from "../bindings/AccountSetup";
import type { OrgAuth } from "../bindings/OrgAuth";
import type { RepoScope } from "../bindings/RepoScope";
import { previewFleet } from "./preview";

const previewConfigEvents = new EventTarget();

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

/** Live data in the desktop app, where commands can act on it. */
export function isLive(view: FleetView): boolean {
  return isTauri && view.source === "live";
}

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (!isTauri) throw new Error(`"${cmd}" is only available in the desktop app`);
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

/** Notify every window only after a settings mutation succeeds. */
async function updateConfig(cmd: string, args?: Record<string, unknown>): Promise<void> {
  await invoke<void>(cmd, args);
  // A notification failure must not turn a completed save into a reported failure.
  await emitEvent("config-updated").catch((error) => console.error("Couldn't notify settings listeners", error));
}

export const commands = {
  getFleet: (): Promise<FleetView> => (isTauri ? invoke("get_fleet") : Promise.resolve(previewFleet())),
  getConfig: (): Promise<AppConfig> =>
    isTauri ? invoke("get_config") : Promise.resolve({ orgs: [], ignoredPeers: [], machinePreferences: {}, demoMode: true, localApi: false, desktopNotifications: false, inventoryPath: null }),
  refreshNow: () => invoke<void>("refresh_now"),
  addOrg: (login: string, auth: OrgAuth, token: string | null, setup: AccountSetup) =>
    updateConfig("add_org", { login, auth, token, setup }),
  getGhStatus: (): Promise<GhStatus> =>
    isTauri
      ? invoke("get_gh_status")
      : Promise.resolve({ installed: false, path: null, host: "github.com", login: null, scopes: [], missingRunnersScope: false, problem: "Only in the desktop app", fix: null }),
  useGhForOrg: (login: string) => updateConfig("use_gh_for_org", { login }),
  updateOrgToken: (login: string, token: string) => updateConfig("update_org_token", { login, token }),
  setOrgScope: (login: string, repoScope: RepoScope, runnerRepos: string[]) =>
    updateConfig("set_org_scope", { login, repoScope, runnerRepos }),
  removeOrg: (login: string) => updateConfig("remove_org", { login }),
  ignorePeer: (nodeId: string) => updateConfig("ignore_peer", { nodeId }),
  unignorePeer: (nodeId: string) => updateConfig("unignore_peer", { nodeId }),
  setMachinePreferences: (id: string, displayName: string, description: string) =>
    updateConfig("set_machine_preferences", { id, displayName, description }),
  setDemoMode: (on: boolean) => updateConfig("set_demo_mode", { on }),
  setMachineMaintenance: (id: string, paused: boolean) => invoke<void>("set_machine_maintenance", { id, paused }),
  getMonitoringEvents: (machineKey: string | null, before: string | null) =>
    invoke<FleetMachineEvent[]>("get_monitoring_events", { machineKey, before }),
  setDesktopNotifications: (enabled: boolean) => updateConfig("set_desktop_notifications", { enabled }),
  testDesktopNotification: () => invoke<void>("test_desktop_notification"),
  openGithubUrl: (url: string) => (isTauri ? invoke<void>("open_github_url", { url }) : Promise.resolve(void window.open(url))),

  // Desired state (inventory/fleet.toml). Checks only read; changes run from a
  // review, and only if the host still matches it.
  desiredRefresh: (host: string | null = null) => invoke<void>("desired_refresh", { host }),
  desiredDoctor: (host: string | null = null) => invoke<void>("desired_doctor", { host }),
  desiredLogs: (runner: string, lines: number) => invoke<string>("desired_logs", { runner, lines }),
  desiredReview: (request: ChangeRequest) => invoke<ReviewView>("desired_review", { request }),
  desiredExecute: (reviewId: string, secrets: ChangeSecrets, confirmation: string | null) =>
    invoke<void>("desired_execute", { reviewId, secrets, confirmation }),
  desiredDismissOperation: () => invoke<void>("desired_dismiss_operation"),

  // Local read-only API for agents
  getLocalApiStatus: (): Promise<LocalApiStatus> =>
    isTauri
      ? invoke("get_local_api_status")
      : Promise.resolve({ enabled: false, url: null, error: "Only in the desktop app", discoveryFile: null }),
  setLocalApi: (on: boolean) => updateConfig("set_local_api", { on }),
};

/** Mirrors `LocalApiStatus` in src-tauri/src/local_api.rs, which has no generated binding. */
export interface LocalApiStatus {
  enabled: boolean;
  url: string | null;
  error: string | null;
  discoveryFile: string | null;
}

export async function onFleetUpdated(handler: (view: FleetView) => void): Promise<() => void> {
  if (!isTauri) return () => {};
  const { listen } = await import("@tauri-apps/api/event");
  return listen<FleetView>("fleet-updated", (e) => handler(e.payload));
}

export async function onConfigUpdated(handler: () => void): Promise<() => void> {
  if (!isTauri) {
    previewConfigEvents.addEventListener("config-updated", handler);
    return () => previewConfigEvents.removeEventListener("config-updated", handler);
  }
  const { listen } = await import("@tauri-apps/api/event");
  return listen("config-updated", handler);
}

export async function emitEvent(name: string): Promise<void> {
  if (!isTauri) {
    if (name === "config-updated") previewConfigEvents.dispatchEvent(new Event(name));
    return;
  }
  const { emit } = await import("@tauri-apps/api/event");
  await emit(name);
}

export function errorText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message;
  return String(e);
}
