import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { previewFleet } from "../../lib/preview";
import { ActivityDetail } from "./ActivityDetail";

const render = (view = previewFleet()) =>
  renderToStaticMarkup(<QueryClientProvider client={new QueryClient()}><ActivityDetail view={view} /></QueryClientProvider>);

describe("activity page", () => {
  it("shows the demo's recorded numbers, labelled cost estimate and recent jobs", () => {
    const html = render();
    expect(html).toContain("Busy by machine");
    expect(html).toContain("mac-mini-02");
    expect(html).toContain("Estimated GitHub-hosted cost");
    expect(html).toContain(`as of ${previewFleet().activity.ratesAsOf}`);
    expect(html.match(/Open .+? on GitHub/g)).toHaveLength(20);
  });

  it("explains an empty history", () => {
    const view = previewFleet();
    view.activity = { ...view.activity, windows: [], recent: [], recordingSince: null };
    expect(render(view)).toContain("No finished jobs recorded in this window.");
  });
});
