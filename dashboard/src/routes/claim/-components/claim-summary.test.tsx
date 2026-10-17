import { renderToString } from "react-dom/server";
import { expect, it } from "vitest";
import { ClaimSummary } from "./claim-summary";

it("the first paint says how long the sandbox has left", () => {
  const html = renderToString(
    <ClaimSummary claim={{ state: "pending", organization: { name: "Agent sandbox", slug: "agent-sandbox" }, servers: 1, projects: 2, hoursLeft: 24 }} />,
  );
  expect(html.replace(/<!-- -->/g, "")).toContain("An agent made it. 1 Server and 2 Projects, gone in 24 hours unless someone keeps it.");
});
