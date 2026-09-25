// @vitest-environment jsdom
import { Suspense } from "react";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { DbProvider } from "@tanstack/react-db";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { parseServiceConfig } from "@ployz/sdk/config";
import { afterEach, expect, it } from "vitest";
import { getEnvironmentDeploymentsCollection, getEnvironmentSavedStateRevisionsCollection } from "#/collections/collections";
import { orgStoreOptions } from "#/collections/org-store";
import { preloadCollection } from "#/collections/query-collection";
import { getDbClient } from "#/collections/scope";
import { Tabs } from "#/components/ui/tabs";
import type { DeploymentProgress } from "#/modules/deployments/deployment-progress";
import { environmentChangeStateOptions } from "#/modules/deployments/environment-change-state.queries";
import { orgStoreSeed, orgStoreTableNames } from "#/test/org-store-tables";
import { canvasRouteSearch } from "../../../-components/deployment-mode";
import { ServiceDeploymentsTab } from "./ServiceDeploymentsTab";

const organizationId = "00000000-0000-4000-8000-000000000001";
const projectId = "00000000-0000-4000-8000-000000000002";
const environmentId = "00000000-0000-4000-8000-000000000003";
const [first, second, third] = ["00000000-0000-4000-8000-000000000011", "00000000-0000-4000-8000-000000000012", "00000000-0000-4000-8000-000000000013"];
const api = "00000000-0000-4000-8000-000000000021";
const worker = "00000000-0000-4000-8000-000000000022";
const createdAt = new Date("2026-09-01T00:00:00Z");
const config = (image: string, privateDns: string) => parseServiceConfig({ version: 2, source: { version: 1, type: "image", image, credentials: { type: "none" } },
  healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns });
const deployment = (id: string, minute: number, message: string, status: "applied" | "failed", runtimeProgress: DeploymentProgress | null) => ({
  id, organizationId, environmentId, triggerOrigin: { origin: "manual", actorId: "user" }, savedStateSnapshotId: id, serviceActionPolicy: null,
  status, inngestRunId: null, coreDeployId: null, retryOfDeploymentId: null, sourcePins: {}, variableProducers: null, deployManifest: null,
  deployPreview: null, runtimeProgress, failureCode: null, failureMessage: null, message, cancellationRequestedAt: null, dispatchRequestedAt: null,
  startedAt: null, finishedAt: null, createdAt: new Date(createdAt.getTime() + minute * 60_000), updatedAt: createdAt,
});
const snapshot = (deploymentId: string, nodeId: string, image: string, privateDns: string) => ({ id: `${deploymentId}:${nodeId}`, organizationId, environmentId,
  environmentDeploymentId: deploymentId, nodeType: "service", nodeId, nodeLineageId: nodeId, configVersion: 1, config: config(image, privateDns), createdAt, updatedAt: createdAt });
// `first` deployed api; `second` only added worker (api Unchanged); `third` failed api's health check, so `first` still serves it.
const failed: DeploymentProgress = { completed: 0, total: 1, outcome: "failed", compensation: [], rows: [{ index: 0, machineId: "m", machineName: "server",
  serviceId: api, runtimeServiceId: null, serviceName: "api", displayName: null, operation: "replace_container", target: "c1", updateOrder: null,
  status: "failed", phase: null, elapsedMs: null, deadlineMs: null, health: null, error: "health check failed", containerId: "c1", startedAt: null, finishedAt: null }] };
const rows = new Map<string, unknown[]>(Object.entries({
  project: [{ id: projectId, organizationId, name: "Shop", slug: "shop", createdAt, updatedAt: createdAt }],
  environment: [{ id: environmentId, projectId, organizationId, name: "Production", namespace: "production", createdAt, updatedAt: createdAt,
    intent: { version: 1, environmentSlug: "production", services: [], volumes: [] } }],
  environment_deployment: [deployment(first, 1, "Ship api", "applied", null), deployment(second, 2, "Add worker", "applied", null),
    deployment(third, 3, "Bump api", "failed", failed)],
  environment_node_config_snapshot: [snapshot(first, api, "nginx:1", "api"), snapshot(second, api, "nginx:1", "api"), snapshot(second, worker, "busybox", "worker"),
    snapshot(third, api, "nginx:2", "api"), snapshot(third, worker, "busybox", "worker")],
}));

async function openTab() {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  for (const table of orgStoreTableNames) queryClient.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed(rows.get(table) ?? []));
  await Promise.all([getEnvironmentDeploymentsCollection, getEnvironmentSavedStateRevisionsCollection].map((get) => preloadCollection(get("acme", scope))));
  await queryClient.fetchQuery(environmentChangeStateOptions("acme", scope, async () => []));
  await queryClient.ensureQueryData(orgStoreOptions("acme", scope));

  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet,
    beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", loader: () => ({ environmentId, organizationId }), component: Outlet });
  const canvas = createRoute({ getParentRoute: () => environment, id: "_canvas", ...canvasRouteSearch, component: Outlet });
  const serviceRoute = createRoute({ getParentRoute: () => canvas, path: "services/$serviceId", component: () => (
    <Suspense><Tabs value="deployments"><ServiceDeploymentsTab organizationSlug="acme" serviceId={api} /></Tabs></Suspense>
  ) });
  const routeTree = root.addChildren([protectedRoute.addChildren([organization.addChildren([
    projectGroup.addChildren([environment.addChildren([canvas.addChildren([serviceRoute])])]),
  ])])]);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: [`/cloud/acme/shop/production/services/${api}`] }) });
  render(<DbProvider client={getDbClient(queryClient)}><QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider></DbProvider>);
  await screen.findByText("History");
  return router;
}

afterEach(cleanup);

it("shows the deployment still serving the service, its changing history, and opens either in Deployment Mode", async () => {
  const router = await openTab();
  const running = screen.getByText("Running").closest("a");
  expect(running?.textContent).toContain("Ship api");
  expect(running?.getAttribute("href")).toBe(`/cloud/acme/shop/production/services/${api}?deployment=${first}&tab=deploy-logs`);
  const history = screen.getAllByText(/Bump api|Add worker/).map((title) => title.closest("a")?.textContent);
  expect(history).toEqual([expect.stringContaining("Failed")]);

  const row = screen.getByText(/Bump api/).closest("a");
  if (!row) throw new Error("Missing history row");
  await act(async () => { fireEvent.click(row); });
  expect(router.state.location.pathname).toBe(`/cloud/acme/shop/production/services/${api}`);
  expect(router.state.location.search).toEqual({ deployment: third });
});
