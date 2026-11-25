import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import type { FleetIssue } from "../../bindings/FleetIssue";
import type { FleetIssueCategory } from "../../bindings/FleetIssueCategory";
import type { FleetIssueFix } from "../../bindings/FleetIssueFix";
import { Attention } from "./Overview";

function issue(id: string, detail: string, fix: FleetIssueFix | null = null, category: FleetIssueCategory = "inventory"): FleetIssue {
  return { id, detail, title: id, category, machineId: null, runnerKey: null,
    stale: false, muted: false, fix };
}

describe("attention rows", () => {
  it("renders findings with no destination as static content", () => {
    const html = renderToStaticMarkup(<Attention issues={[issue("inventory", "Inventory is invalid"), issue("github", "GitHub unavailable"), issue("tailnet", "Tailscale unavailable")]} />);
    expect(html).toContain("Inventory is invalid");
    expect(html).toContain("GitHub unavailable");
    expect(html).toContain("Tailscale unavailable");
    expect(html).not.toContain("<button");
  });

  it("keeps actionable findings as keyboard accessible buttons", () => {
    const html = renderToStaticMarkup(<Attention issues={[issue("github", "Missing auth", "organizationSignIn"), issue("org", "No organization", "addOrganization")]} />);
    expect(html.match(/<button type="button"/g)).toHaveLength(2);
    expect(html).toContain("Missing auth");
    expect(html).toContain("Add organization");
  });

  it("leaves alerts, history storage and the running change to their own sections", () => {
    const issues = [issue("alert", "Offline", null, "alert"), issue("storage", "Can't save", null, "storage"), issue("operation", "Failed", null, "operation")];
    expect(renderToStaticMarkup(<Attention issues={issues} />)).toBe("");
  });
});

describe("inventory that fails to load", () => {
  it("says why, with the error and the path, on the first screen with and without an organization", async () => {
    const { QueryClient, QueryClientProvider } = await import("@tanstack/react-query");
    const { previewFleet } = await import("../../lib/preview");
    const { Overview } = await import("./Overview");
    const failed = previewFleet();
    failed.source = "live";
    failed.machines = [];
    failed.plan = { ...failed.plan, inventoryError: "cannot load /fake/fleet.toml: No such file", inventoryPath: "/fake/fleet.toml" };
    for (const view of [failed, { ...failed, orgs: [] }]) {
      const html = renderToStaticMarkup(
        <QueryClientProvider client={new QueryClient()}><Overview view={view} /></QueryClientProvider>,
      );
      expect(html).toContain("The inventory couldn&#x27;t be loaded");
      expect(html).toContain("No such file");
      expect(html).toContain("/fake/fleet.toml");
    }
  });
});
