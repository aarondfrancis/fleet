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
