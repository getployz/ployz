// @vitest-environment jsdom
import { Suspense } from "react";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { DbProvider } from "@tanstack/react-db";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { Schema } from "effect";
import { parseServiceConfig } from "@ployz/sdk/config";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import * as preference from "#/auth/open-started-deployments";
import { getEnvironmentDeploymentsCollection, getEnvironmentSavedStateRevisionsCollection } from "#/collections/collections";
import { orgStoreOptions } from "#/collections/org-store";
import { preloadCollection } from "#/collections/query-collection";
import { getDbClient } from "#/collections/scope";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import * as deploymentCollections from "#/modules/deployments/deployment.collection";
import * as deploymentFunctions from "#/modules/deployments/deployment.functions";
import type { DeploymentProgress, DeploymentProgressRow } from "#/modules/deployments/deployment-progress";
import * as preflight from "#/modules/runtime/deploy-target-preflight";
import * as restore from "#/modules/environment-design/working-document-restore.functions";
import { asTestDouble } from "#/lib/test-double";
import { SidebarProvider } from "#/components/ui/sidebar";
import { environmentChangeStateOptions } from "#/modules/deployments/environment-change-state.queries";
import { defaultServicePolicy } from "#/modules/environment-design/service-policy";
import { RuntimeProvider } from "#/providers/runtime-provider";
import { orgStoreSeed, orgStoreTableNames } from "#/test/org-store-tables";
import { serviceSearchSchema } from "../services/$serviceId/-components/service-pages";
import { canvasRouteSearch } from "./deployment-mode";
import { EnvironmentCanvasScene } from "./EnvironmentCanvasScene";
import { ENVIRONMENT_INDEX_ROUTE_TO, ENVIRONMENT_SERVICE_ROUTE_TO } from "./environment-route-paths";
import { Route as deploymentsRoute } from "../deployments";

const organizationId = "00000000-0000-4000-8000-000000000001";
const projectId = "00000000-0000-4000-8000-000000000002";
const environmentId = "00000000-0000-4000-8000-000000000003";
const [previous, attemptId, failedId, runningId, replaceFailedId] = ["a0000000-0000-4000-8000-000000000011", "b0000000-0000-4000-8000-000000000012",
  "c0000000-0000-4000-8000-000000000013", "d0000000-0000-4000-8000-000000000014", "e0000000-0000-4000-8000-000000000015"];
const [api, old, worker, web] = ["00000000-0000-4000-8000-000000000021", "00000000-0000-4000-8000-000000000022", "00000000-0000-4000-8000-000000000023", "00000000-0000-4000-8000-000000000024"];
const params = { organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production" };
const createdAt = new Date("2026-09-01T00:00:00Z");
const config = (privateDns: string) => parseServiceConfig({ version: 2, source: { version: 1, type: "image", image: "nginx:1", credentials: { type: "none" } },
  healthcheck: { type: "none" }, restartPolicy: "unless-stopped", privateDns });
const intentService = (id: string, slug: string) => {
  const { env: _env, mounts: _mounts, ...authored } = config(slug);
  return { id, lineageId: id, slug, config: authored, variables: [], volumeAttachments: [] };
};
const service = (id: string, name: string) => ({ id, organizationId, projectId, environmentId, lineageId: id, name, policy: defaultServicePolicy,
  hasRegistryCredential: false, firstDeployedAt: createdAt, createdAt, updatedAt: createdAt });
const deployment = (id: string, minute: number, runtimeProgress: DeploymentProgress | null, status = "applied") => ({
  id, organizationId, environmentId, triggerOrigin: { origin: "manual", actorId: "user" }, savedStateSnapshotId: id, serviceActionPolicy: null,
  status, inngestRunId: null, coreDeployId: null, retryOfDeploymentId: null, sourcePins: {}, variableProducers: null, deployManifest: null,
  deployPreview: null, runtimeProgress, failureCode: null, failureMessage: null, message: null, cancellationRequestedAt: null, dispatchRequestedAt: null,
  startedAt: null, finishedAt: null, createdAt: new Date(createdAt.getTime() + minute * 60_000), updatedAt: createdAt,
});
const snapshot = (deploymentId: string, nodeId: string, privateDns: string) => ({ id: `${deploymentId}:${nodeId}`, organizationId, environmentId,
  environmentDeploymentId: deploymentId, nodeType: "service", nodeId, nodeLineageId: nodeId, configVersion: 1, config: config(privateDns), createdAt, updatedAt: createdAt });
// The attempt removed `old` (its row has no serviceId on the record side), failed `web`'s health check and left `api` unchanged; `worker` came later.
const removal: DeploymentProgressRow = { index: 0, machineId: "m", machineName: "server", serviceId: null, runtimeServiceId: null, serviceName: "old", displayName: null,
  operation: "remove_container", target: null, updateOrder: null, status: "completed", phase: null, elapsedMs: null, deadlineMs: null, health: null, error: null,
  containerId: null, startedAt: 0, finishedAt: 3_000 };
const healthFailure: DeploymentProgressRow = { ...removal, index: 1, serviceId: web, serviceName: "web", operation: "replace_container", status: "failed",
  error: "Health check timed out", containerId: "4e7a19c", startedAt: 3_000, finishedAt: 63_000 };
// A later attempt failed replacing `api`: the health check failed in container c0ffee.
const failedReplace: DeploymentProgressRow = { ...removal, serviceId: api, serviceName: "api", operation: "replace_container", status: "failed",
  error: "Health check timed out after 60s", containerId: "c0ffee" };
const rows = new Map<string, unknown[]>(Object.entries({
  project: [{ id: projectId, organizationId, name: "Shop", slug: "shop", createdAt, updatedAt: createdAt }],
  environment: [{ id: environmentId, projectId, organizationId, name: "Production", namespace: "production", revision: "r1", createdAt, updatedAt: createdAt,
    intent: { version: 1, environmentSlug: "production", services: [intentService(api, "api"), intentService(web, "web"), intentService(worker, "worker")], volumes: [] } }],
  environment_summary: [{ id: environmentId, projectId, organizationId, name: "Production", namespace: "production", createdAt }],
  service: [service(api, "api"), service(old, "old"), service(web, "web"), service(worker, "worker")],
  environment_deployment: [deployment(previous, 1, null),
    deployment(attemptId, 2, { completed: 1, total: 2, outcome: "failed", rows: [removal, healthFailure], compensation: [] }, "failed"),
    deployment(replaceFailedId, 3, { completed: 0, total: 1, outcome: "failed", rows: [failedReplace], compensation: [] }, "failed")],
  environment_node_config_snapshot: [snapshot(previous, api, "api"), snapshot(previous, old, "old"), snapshot(previous, web, "web"),
    snapshot(attemptId, api, "api"), snapshot(attemptId, web, "web"), snapshot(replaceFailedId, api, "api")],
}));
const card = (name: string) => screen.getAllByText(name)[0]?.closest("[data-canvas-node]");

async function openCanvas({ extra = {}, path = "/cloud/acme/shop/production", changeStates = [] }: {
  extra?: Record<string, unknown[]>; path?: string; changeStates?: EnvironmentChangeStateProjection[];
} = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  for (const table of orgStoreTableNames) queryClient.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed([...rows.get(table) ?? [], ...extra[table] ?? []]));
  // The failed attempt's event log is finished and empty, so Deploy logs reads it from cache.
  queryClient.setQueryData(["collections", "session", "user", "acme", "deployment_logs", replaceFailedId], { events: [], finished: true });
  // The change-state projection stamps its version from these tables, then reads the given states (none by default).
  await Promise.all([getEnvironmentDeploymentsCollection, getEnvironmentSavedStateRevisionsCollection].map((get) => preloadCollection(get("acme", scope))));
  await queryClient.fetchQuery(environmentChangeStateOptions("acme", scope, async () => changeStates));
  await queryClient.ensureQueryData(orgStoreOptions("acme", scope));

  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet,
    beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug",
    component: () => <RuntimeProvider organizationSlug="acme"><Outlet /></RuntimeProvider> });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    loader: () => ({ environmentId, organizationId }), component: Outlet });
  const canvas = createRoute({ getParentRoute: () => environment, id: "_canvas", ...canvasRouteSearch,
    component: () => <Suspense fallback={<p>Loading canvas</p>}><EnvironmentCanvasScene /></Suspense> });
  const index = createRoute({ getParentRoute: () => canvas, path: "/", component: () => null });
  const serviceRoute = createRoute({ getParentRoute: () => canvas, path: "services/$serviceId",
    validateSearch: Schema.toStandardSchemaV1(serviceSearchSchema), component: () => <p>Live service panel</p> });
  const deployments = createRoute({ getParentRoute: () => environment, path: "deployments", beforeLoad: (context) => { deploymentsRoute.options.beforeLoad?.(context as never); } });
  const routeTree = root.addChildren([protectedRoute.addChildren([organization.addChildren([
    projectGroup.addChildren([environment.addChildren([canvas.addChildren([index, serviceRoute]), deployments])]),
  ])])]);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: [path] }) });
  render(<DbProvider client={getDbClient(queryClient)}><QueryClientProvider client={queryClient}><SidebarProvider><RouterProvider router={router} /></SidebarProvider></QueryClientProvider></DbProvider>);
  await screen.findAllByText("worker");
  return router;
}

const enterDeploymentMode = (router: Awaited<ReturnType<typeof openCanvas>>, deployment = attemptId) =>
  act(() => router.navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: { deployment } }));

async function openNode(nodeId: string) {
  const link = await waitFor(() => {
    const found = document.querySelector<HTMLAnchorElement>(`a[data-canvas-node="${nodeId}"]`);
    if (!found) throw new Error(`Missing link to ${nodeId}`);
    return found;
  });
  await act(async () => { fireEvent.click(link); });
}

// Re-spied per test: the apply zone's afterEach restores every mock.
const setOpenStarted = () => vi.mocked(preference.setOpenStartedDeployments);
beforeEach(() => {
  vi.spyOn(preference, "setOpenStartedDeployments").mockReset().mockResolvedValue();
  vi.stubGlobal("EventSource", class { addEventListener() {} removeEventListener() {} close() {} });
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("deployment mode on the environment canvas", () => {
  it("redraws the canvas as the attempt saw it and returns to live", async () => {
    const router = await openCanvas();
    expect(screen.queryByText("Back to editor")).toBeNull();
    expect(screen.queryAllByText("Removed")).toEqual([]);

    await enterDeploymentMode(router);
    expect((await screen.findAllByText("Back to editor")).length).toBeGreaterThan(0);
    // Deleted since and removed by the attempt: still drawn. Created afterwards: hidden.
    expect(screen.getAllByText("old")[0]?.closest("[data-canvas-node]")?.textContent).toContain("Removed");
    const unchanged = screen.getAllByText("api")[0]?.closest("[data-canvas-node]");
    expect(unchanged?.textContent).toContain("Unchanged");
    expect(unchanged?.getAttribute("data-dimmed")).toBe("true");
    expect(screen.queryAllByText("worker")).toEqual([]);
    // Read-only: no Create, no change controls.
    expect(screen.queryByRole("button", { name: "Create" })).toBeNull();

    // Navigating inside the canvas keeps the mode, and the editable live panel stays closed.
    await act(() => router.navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: api }, search: {} }));
    expect(router.state.location.search).toMatchObject({ deployment: attemptId });
    expect(screen.queryByText("Live service panel")).toBeNull();
    await act(() => router.navigate({ to: ENVIRONMENT_INDEX_ROUTE_TO, params, search: (prev) => prev }));

    const [backToEditor] = screen.getAllByText("Back to editor");
    if (!backToEditor) throw new Error("Missing Back to editor");
    await act(async () => { fireEvent.click(backToEditor); });
    expect((await screen.findAllByText("worker")).length).toBeGreaterThan(0);
    expect(router.state.location.search).not.toHaveProperty("deployment");
    expect(screen.queryAllByText("Removed")).toEqual([]);
  });

  it("draws a node whose service row and position are both gone by its snapshot's name, clear of the origin", async () => {
    const gone = "00000000-0000-4000-8000-000000000025";
    const goneAttempt = "f0000000-0000-4000-8000-000000000016";
    const router = await openCanvas({ extra: {
      environment_deployment: [deployment(goneAttempt, 4, null)],
      environment_node_config_snapshot: [snapshot(goneAttempt, api, "api"), snapshot(goneAttempt, gone, "billing")],
      environment_canvas_node_position: [{ id: "00000000-0000-4000-8000-000000000031", organizationId, environmentId, resourceType: "service", resourceId: api, x: 0, y: 0, createdAt, updatedAt: createdAt }],
    } });

    await enterDeploymentMode(router, goneAttempt);
    expect(card("billing")).toBeTruthy();
    expect(screen.queryByText(gone)).toBeNull();
    const at = (nodeId: string) => document.querySelector(`.react-flow__node[data-id="${nodeId}"]`)?.getAttribute("style");
    expect(at(api)).toMatch(/translate\(0px, ?0px\)/);
    expect(at(gone)).not.toMatch(/translate\(0px, ?0px\)/);
  });

  it("shows Build → Deploy and the tail on nodes the attempt changed, and nothing new on live nodes", async () => {
    const router = await openCanvas();
    expect(card("web")?.textContent).not.toContain("Failed");
    expect(document.querySelector("[data-stage], [data-tail]")).toBeNull();

    await enterDeploymentMode(router);
    await screen.findAllByText("Removed");
    const failed = card("web");
    expect(failed?.textContent).toContain("Failed");
    expect(failed?.querySelector('[data-stage="Build"]')?.textContent).toBe("Build —");
    expect(failed?.querySelector('[data-stage="Deploy"]')?.textContent).toBe("Deploy 1m 0s");
    expect(failed?.querySelector("[data-tail]")?.textContent).toBe("server · Health check timed out");
    expect(card("old")?.querySelector('[data-stage="Deploy"]')?.textContent).toBe("Deploy 3s");
    expect(card("old")?.querySelector("[data-tail]")?.textContent).toBe("server · Removing container · done");
    // Unchanged: name and outcome only, dimmed.
    expect(card("api")?.querySelector("[data-stage], [data-tail]")).toBeNull();
    expect(card("api")?.textContent).toBe("apiUnchanged");
  });

  it("opens the panel from a desktop canvas node, which takes pointer events", async () => {
    const router = await openCanvas();
    await enterDeploymentMode(router, replaceFailedId);
    const node = await waitFor(() => {
      const found = document.querySelector<HTMLElement>(`.react-flow__node[data-id="${api}"]`);
      if (!found) throw new Error("Missing React Flow node");
      return found;
    });
    // React Flow sets pointer-events: none on a node with no click handler, so a real click falls through to the pane.
    expect(node.style.pointerEvents).not.toBe("none");
    const link = node.querySelector("a");
    if (!link) throw new Error("Missing node link");
    await act(async () => { fireEvent.click(link); });
    await waitFor(() => expect(screen.getByRole("tab", { name: "Deploy logs" }).getAttribute("aria-selected")).toBe("true"));
  });

  it("opens a node's read-only panel on the tab its outcome calls for, with the tab in the URL", async () => {
    const router = await openCanvas();
    await enterDeploymentMode(router, replaceFailedId);
    await openNode(api);

    // A failed rollout lands on Deploy logs; the panel has only the Deployment Mode tabs.
    await waitFor(() => expect(screen.getByRole("tab", { name: "Deploy logs" }).getAttribute("aria-selected")).toBe("true"));
    await waitFor(() => expect(router.state.location.search).toMatchObject({ deployment: replaceFailedId, tab: "deploy-logs" }));
    expect(screen.getAllByRole("tab").map((tab) => tab.textContent)).toEqual(["Details", "Build logs", "Deploy logs"]);
    // A prebuilt image built nothing.
    expect(screen.getByRole("tab", { name: "Build logs" }).getAttribute("aria-disabled")).toBe("true");
    expect(screen.queryByText("Live service panel")).toBeNull();
    // The header's section picker stands in for the tabs on mobile, so it agrees.
    fireEvent.click(screen.getByRole("button", { name: "Project navigation" }));
    const picker = await screen.findByRole("dialog", { name: "Project navigation" });
    expect(within(picker).getAllByRole("link", { name: "Build logs" })[0]?.getAttribute("aria-disabled")).toBe("true");
    fireEvent.keyDown(picker, { key: "Escape" });

    fireEvent.click(screen.getByRole("tab", { name: "Details" }));
    await waitFor(() => expect(router.state.location.search).toMatchObject({ deployment: replaceFailedId, tab: "details" }));
    expect(screen.getByText("Health check timed out after 60s")).toBeTruthy();
    expect(screen.getByText("c0ffee")).toBeTruthy();
    expect(screen.getByText("0 variables (as deployed)")).toBeTruthy();
    expect(within(screen.getByRole("region", { name: "Resource inspector" })).getByText("nginx:1")).toBeTruthy();
    // Read-only: nothing to edit or save.
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  it("opens an unchanged node on Details and keeps the live panel for Editor Mode", async () => {
    const router = await openCanvas();
    await enterDeploymentMode(router);
    await openNode(api);
    expect(await screen.findByText(/Unchanged in this deployment/)).toBeTruthy();
    await waitFor(() => expect(router.state.location.search).toMatchObject({ tab: "details" }));

    await act(() => router.navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: api }, search: { deployment: undefined, tab: undefined } }));
    expect(await screen.findByText("Live service panel")).toBeTruthy();
    expect(screen.queryAllByRole("tab")).toEqual([]);
  });

  it("leaves the mode on browser Back", async () => {
    const router = await openCanvas();
    await enterDeploymentMode(router);
    await screen.findAllByText("Removed");
    await act(async () => { router.history.back(); });
    expect((await screen.findAllByText("worker")).length).toBeGreaterThan(0);
    expect(screen.queryByText("Back to editor")).toBeNull();
  });
});

describe("the deploy bar", () => {
  const bar = () => within(screen.getByRole("group", { name: "Deploy bar" }));
  const click = (element: HTMLElement) => act(async () => { fireEvent.click(element); });

  it("switches between the Editor and a deployment picked from the list", async () => {
    const router = await openCanvas();
    expect(bar().getByRole("link", { name: "Editor" }).getAttribute("data-active")).toBe("true");

    await click(bar().getByRole("button", { name: "Deployments" }));
    expect(router.state.location.search).toMatchObject({ deploymentList: true });
    const list = within(await screen.findByRole("navigation", { name: "Deployments" }));
    // Editor first, then deployments newest first.
    expect(list.getAllByRole("link").map((row) => row.textContent)).toEqual([
      expect.stringContaining("Editor"), expect.stringContaining("e0000000"), expect.stringContaining("b0000000"), expect.stringContaining("a0000000"),
    ]);

    await click(list.getByRole("link", { name: /b0000000/ }));
    expect(router.state.location.search).toEqual({ deployment: attemptId });
    expect(await screen.findByRole("button", { name: /Deployment b0000000/ })).toBeTruthy();
    expect(screen.queryByRole("navigation", { name: "Deployments" })).toBeNull();

    await click(bar().getByRole("link", { name: "Editor" }));
    expect(router.state.location.search).toEqual({});
    expect((await screen.findAllByText("worker")).length).toBeGreaterThan(0);
  });

  it("opens a running attempt directly and offers Cancel, and Retry on a failed one", async () => {
    const router = await openCanvas({ extra: {
      environment_deployment: [deployment(failedId, 3, null, "failed"), deployment(runningId, 4, null, "deploying")],
      environment_node_config_snapshot: [snapshot(runningId, api, "api"), snapshot(runningId, worker, "worker")],
    } });
    await click(bar().getByRole("link", { name: /Deploying 0\/3/ }));
    expect(router.state.location.search).toEqual({ deployment: runningId });
    expect(await bar().findByRole("button", { name: "Cancel" })).toBeTruthy();
    expect(bar().queryByRole("button", { name: "Retry" })).toBeNull();

    await click(bar().getByRole("button", { name: /Deployment d0000000/ }));
    await click(within(await screen.findByRole("navigation", { name: "Deployments" })).getByRole("link", { name: /c0000000/ }));
    expect(await bar().findByRole("button", { name: "Retry" })).toBeTruthy();
    expect(bar().queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("remembers leaving your own running attempt and reopening it", async () => {
    const router = await openCanvas({ extra: {
      environment_deployment: [deployment(runningId, 4, null, "deploying")],
      environment_node_config_snapshot: [snapshot(runningId, api, "api")],
    } });
    expect(setOpenStarted()).not.toHaveBeenCalled();
    await click(bar().getByRole("link", { name: /Deploying/ }));
    expect(setOpenStarted()).toHaveBeenLastCalledWith(true);
    await click(bar().getByRole("link", { name: "Editor" }));
    expect(setOpenStarted()).toHaveBeenLastCalledWith(false);
    await click(bar().getByRole("link", { name: /Deploying/ }));
    expect(setOpenStarted()).toHaveBeenLastCalledWith(true);
    // Leaving a finished attempt keeps the preference.
    await enterDeploymentMode(router);
    await click(bar().getByRole("link", { name: "Editor" }));
    expect(setOpenStarted()).toHaveBeenCalledTimes(3);
  });

  it("never opens a Git-triggered attempt or counts it as yours", async () => {
    await openCanvas({ extra: {
      environment_deployment: [{ ...deployment(runningId, 4, null, "deploying"), triggerOrigin: {
        origin: "github", deliveryId: "delivery", branchEvaluationRevision: 1, installationId: 1, repositoryId: 1,
      } }],
      environment_node_config_snapshot: [snapshot(runningId, api, "api")],
    } });
    expect(screen.queryByText("Back to editor")).toBeNull();
    await click(bar().getByRole("link", { name: /Deploying/ }));
    await click(bar().getByRole("link", { name: "Editor" }));
    expect(setOpenStarted()).not.toHaveBeenCalled();
  });

  it("stays usable while a service panel is open", async () => {
    const router = await openCanvas();
    await act(() => router.navigate({ to: ENVIRONMENT_SERVICE_ROUTE_TO, params: { ...params, serviceId: api } }));
    await screen.findByText("Live service panel");
    expect(screen.getByRole("group", { name: "Deploy bar" }).closest("[inert]")).toBeNull();
    await click(bar().getByRole("button", { name: "Deployments" }));
    expect(await screen.findByRole("navigation", { name: "Deployments" })).toBeTruthy();
  });

  it("opens the list as a bottom sheet on mobile", async () => {
    vi.stubGlobal("innerWidth", 375);
    await openCanvas();
    await click(bar().getByRole("button", { name: "Deployments" }));
    const sheet = await screen.findByRole("dialog", { name: "Deployments" });
    expect(sheet.getAttribute("data-swipe-direction")).toBe("down");
    expect(within(sheet).getByRole("link", { name: /b0000000/ })).toBeTruthy();
  });

  it("leaves Deployment Mode on Esc", async () => {
    const router = await openCanvas();
    await enterDeploymentMode(router);
    await screen.findAllByText("Removed");
    await act(async () => { fireEvent.keyDown(document.body, { key: "Escape" }); });
    expect(router.state.location.search).toEqual({});
  });

  it("redirects the old Deployments page to the canvas with the list open", async () => {
    const router = await openCanvas({ path: "/cloud/acme/shop/production/deployments" });
    expect(router.state.location.pathname).toBe("/cloud/acme/shop/production");
    expect(router.state.location.search).toEqual({ deploymentList: true });
    expect(await screen.findByRole("navigation", { name: "Deployments" })).toBeTruthy();
  });
});

describe("the apply zone", () => {
  const bar = () => within(screen.getByRole("group", { name: "Deploy bar" }));
  const click = (element: HTMLElement) => act(async () => { fireEvent.click(element); });
  // Applied State runs every service as authored except `api`, which ran two replicas: one pending change.
  const appliedNode = (nodeId: string, serviceConfig: ReturnType<typeof config>) =>
    ({ nodeType: "service" as const, nodeId, nodeLineageId: nodeId, revisionId: null, config: serviceConfig });
  const applied = { token: "applied",
    nodes: [appliedNode(api, { ...config("api"), replicas: 2 }), appliedNode(web, config("web")), appliedNode(worker, config("worker"))] };
  const pending: EnvironmentChangeStateProjection = { environmentId, saved: null, applied, deploymentEvidence: null };
  const submit = () => vi.mocked(deploymentFunctions.submitReviewedPublicationServerFn);
  beforeEach(() => {
    vi.spyOn(preflight, "getDeployTargetPreflight").mockReturnValue({ ok: true });
    vi.spyOn(deploymentFunctions, "listLatestOrganizationEnvironmentChangeStatesServerFn").mockResolvedValue([pending]);
    vi.spyOn(deploymentFunctions, "submitReviewedPublicationServerFn").mockResolvedValue({ state: "deployment_queued", deploymentId: "f0000000-0000-4000-8000-000000000016" });
  });
  afterEach(() => { vi.restoreAllMocks(); });

  it("turns the whole bar staged-intent and opens the existing review from Details", async () => {
    await openCanvas({ changeStates: [pending] });
    expect(await bar().findByText("Apply 1 change")).toBeTruthy();
    expect(screen.getByRole("group", { name: "Deploy bar" }).querySelector(".apply-zone")).toBeTruthy();
    expect(bar().getByRole("button", { name: /^Deploy(⇧\+Enter)?$/ }).textContent).toBe("Deploy⇧+Enter");

    await click(bar().getByRole("button", { name: "Details" }));
    expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
    await click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog", { name: "Environment changes" })).toBeNull();
    expect(document.activeElement).toBe(bar().getByRole("button", { name: "Details" }));
  });

  it("deploys from the button and from ⇧+Enter, but never from multi-line text", async () => {
    await openCanvas({ changeStates: [pending] });
    await click(await bar().findByRole("button", { name: /^Deploy(⇧\+Enter)?$/ }));
    await waitFor(() => expect(submit()).toHaveBeenCalledWith({ data: expect.objectContaining({ intent: "manual_deploy" }) }));

    const textarea = document.body.appendChild(document.createElement("textarea"));
    await act(async () => { fireEvent.keyDown(textarea, { key: "Enter", shiftKey: true }); });
    textarea.remove();
    await act(async () => { fireEvent.keyDown(document.body, { key: "Enter" }); });
    expect(submit()).toHaveBeenCalledTimes(1);
    await act(async () => { fireEvent.keyDown(document.body, { key: "Enter", shiftKey: true }); });
    await waitFor(() => expect(submit()).toHaveBeenCalledTimes(2));
  });

  it("discards every change from the ⋮ menu", async () => {
    // The server restores `api` to its applied two replicas.
    const [environment] = rows.get("environment") as [{ intent: { services: ReturnType<typeof intentService>[] } }];
    const restored = { ...environment, revision: "r2", intent: { ...environment.intent, services: environment.intent.services.map((node) =>
      node.id === api ? { ...node, config: { ...node.config, replicas: 2 } } : node) } };
    const discard = vi.spyOn(restore, "discardEnvironmentChangesServerFn").mockResolvedValue(
      asTestDouble<Awaited<ReturnType<typeof restore.discardEnvironmentChangesServerFn>>>()({ data: restored }));
    vi.spyOn(deploymentCollections, "reconcileDeploymentCollections").mockResolvedValue(undefined);
    await openCanvas({ changeStates: [pending] });
    await click(await bar().findByRole("button", { name: "More change actions" }));
    await click(await screen.findByRole("menuitem", { name: "Discard all changes" }));
    await waitFor(() => expect(bar().queryByText(/Apply/)).toBeNull());
    expect(discard).toHaveBeenCalledWith({ data: expect.objectContaining({ environmentId, revision: "r1", command: { kind: "all" } }) });
  });

  it("keeps the changes during a Git-triggered deployment and shows a Deploy behind it as Queued", async () => {
    const fromPush = { ...deployment(runningId, 4, null, "deploying"),
      triggerOrigin: { origin: "github", deliveryId: "delivery", branchEvaluationRevision: 1, installationId: 1, repositoryId: 1 } };
    // The pushed run deploys Saved State, which is Applied State here, so the canvas edit stays pending.
    const running: EnvironmentChangeStateProjection = { ...pending, deploymentEvidence: {
      id: runningId, savedStateSnapshotId: runningId, status: "deploying", token: "pushed", createdAt, nodes: applied.nodes } };
    vi.spyOn(deploymentFunctions, "listLatestOrganizationEnvironmentChangeStatesServerFn").mockResolvedValue([running]);
    const router = await openCanvas({ changeStates: [running], extra: {
      environment_deployment: [fromPush, deployment(failedId, 5, null, "queued")],
    } });
    expect(await bar().findByText("Apply 1 change")).toBeTruthy();
    expect(bar().getByRole("link", { name: /Deploying/ })).toBeTruthy();

    await click(bar().getByRole("link", { name: "Queued" }));
    expect(router.state.location.search).toEqual({ deployment: failedId });
    // Deployment Mode is read-only: no apply zone.
    expect(await bar().findByRole("button", { name: /Deployment c0000000/ })).toBeTruthy();
    expect(bar().queryByText(/Apply/)).toBeNull();
    // Queued for the next trigger: it can be dispatched from the bar.
    const dispatch = vi.spyOn(deploymentFunctions, "dispatchQueuedEnvironmentDeploymentServerFn").mockResolvedValue({ state: "dispatched" });
    await click(bar().getByRole("button", { name: "Deploy now" }));
    expect(dispatch).toHaveBeenCalledWith({ data: { organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production" } });
  });

  it("uses fewer words on mobile: the count opens Details and ⋮ keeps Discard", async () => {
    vi.stubGlobal("innerWidth", 375);
    await openCanvas({ changeStates: [pending] });
    const count = await bar().findByRole("button", { name: "Details, Apply 1" });
    expect(count.textContent).toBe("Apply 1");
    expect(bar().getByRole("button", { name: /^Deploy(⇧\+Enter)?$/ }).textContent).toBe("Deploy");
    await click(bar().getByRole("button", { name: "More change actions" }));
    expect(await screen.findByRole("menuitem", { name: "Discard all changes" })).toBeTruthy();
    expect(screen.queryByRole("menuitem", { name: "Details" })).toBeNull();
    await act(async () => { fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" }); });
    await click(count);
    expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
  });
});
