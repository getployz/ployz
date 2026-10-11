// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import type { ReactNode } from "react";
import type { DeploymentView, JsonValue } from "@ployz/sdk";
import { afterEach, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { deploymentQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { toolOutcome, ToolRow } from "./tool-row";

afterEach(() => { cleanup(); });

const scope = { queryClient: new QueryClient(), sessionId: "s", userId: "u" };
const deployed = { written: "deployment", id: "d3", number: 3, status: "running", services: [], remove: false, started_at: null, ended_at: null };
const call = (name: string, output: JsonValue | undefined) =>
  ({ type: "tool-call" as const, id: "c1", name, arguments: "{}", state: "input-complete" as const, output });

async function show(row: (client: QueryClient) => ReactNode, seed?: (client: QueryClient) => void) {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false } } });
  seed?.(client);
  const router = createRouter({ routeTree: createRootRoute({ component: () => row(client) }), history: createMemoryHistory({ initialEntries: ["/"] }) });
  await router.load();
  render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  await waitFor(() => expect(document.body.textContent).not.toBe(""));
}

it("draws a Deployment the agent started in the Deployment Page's words, and links it once the Store has it", async () => {
  const outcome = toolOutcome(call("deploy", { ok: true, value: deployed, nothing_destroyed: true }), undefined);
  const row = () => <ToolRow organizationSlug="acme" scope={scope} name="deploy" outcome={outcome} done />;
  await show(row);
  expect(screen.getByText("Deploying")).toBeTruthy();
  expect(screen.queryByRole("link")).toBeNull();
  expect(screen.getByText("Didn't ask. Nothing destroyed.")).toBeTruthy();
  cleanup();

  const applied = asTestDouble<StoreResult<{ view: "deployment" } & DeploymentView>>()({
    ok: true,
    value: {
      view: "deployment", id: "d3", number: 3, status: "applied", services: [], remove: false, started_at: 100, ended_at: 172, outcome: null,
      environment: { id: "e1", project: "shop", name: "production", revision: 4 },
    },
  });
  await show(row, (client) => client.setQueryData(storeViewOptions("acme", scope, deploymentQuery("d3")).queryKey, applied));
  expect(screen.getByText("Deployed")).toBeTruthy();
  expect(screen.getByText("1m 12s")).toBeTruthy();
  expect(screen.getByRole("link", { name: "Deployment #3" }).getAttribute("href")).toBe("/cloud/acme/shop/production/deployments/d3");
});

it("reads the result part's JSON after a reload, and says nothing about destruction the gate did not rule out", async () => {
  const outcome = toolOutcome(call("deploy", undefined), { type: "tool-result", toolCallId: "c1", content: JSON.stringify({ ok: true, value: deployed }), state: "complete" });
  await show(() => <ToolRow organizationSlug="acme" scope={scope} name="deploy" outcome={outcome} done />);
  expect(screen.getByText("Deploying")).toBeTruthy();
  expect(screen.queryByText("Didn't ask. Nothing destroyed.")).toBeNull();
});

it("gives a denial's reason, and a refusal's message", async () => {
  const denied = toolOutcome(call("deploy", { ok: false, refusal: { code: "approval_denied", message: "denied", details: { approval: { reason: "keep the data" } } } }), undefined);
  await show(() => <ToolRow organizationSlug="acme" scope={scope} name="deploy" outcome={denied} done />);
  expect(screen.getByText("Denied: keep the data")).toBeTruthy();
  cleanup();

  const refused = toolOutcome(call("service_rm", { ok: false, refusal: { code: "not_found", message: "No service named api." } }), undefined);
  await show(() => <ToolRow organizationSlug="acme" scope={scope} name="service_rm" outcome={refused} done />);
  expect(screen.getByText("No service named api.")).toBeTruthy();
});

it("names any other call by its command", async () => {
  const outcome = toolOutcome(call("project_ls", [{ name: "shop" }]), undefined);
  await show(() => <ToolRow organizationSlug="acme" scope={scope} name="project_ls" outcome={outcome} done />);
  expect(screen.getByText("ployz project ls")).toBeTruthy();
});
