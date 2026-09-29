// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider, useSearch,
} from "@tanstack/react-router";
import { Schema } from "effect";
import { afterEach, expect, it, vi } from "vitest";
import { InspectorPresentation } from "./CanvasInspectorHeader";
import { LegacyDeploymentsList, StoreDeploymentsList } from "./DeploymentsList";
import * as navigation from "./environment-node-navigation";
import * as deploymentCollections from "#/modules/deployments/deployment.collection";
import * as history from "#/modules/deployments/deployment-history.queries";
import { asTestDouble } from "#/lib/test-double";
import * as storeViews from "#/modules/config-store/store-view.queries";
// Over the Store: a CLI Deploy that targeted worker, a full one from the dashboard, and a failed upload.
vi.spyOn(storeViews, "useStoreDeployments").mockReturnValue(asTestDouble<ReturnType<typeof storeViews.useStoreDeployments>>()({
  data: { pages: [{ next_cursor: null, deployments: [
    { id: "d3", number: 3, status: "running", services: ["worker"], upload: null },
    { id: "d2", number: 2, status: "applied", services: [], upload: null },
    { id: "d1", number: 1, status: "failed", services: ["api"], upload: { digest: "x", base: { commit: "abc1234ff", changed: false }, uploader: "nick" } },
  ] }] },
  hasNextPage: false, isFetchingNextPage: false, fetchNextPage: vi.fn(),
}));
vi.spyOn(storeViews, "useStoreView").mockReturnValue(asTestDouble<ReturnType<typeof storeViews.useStoreView>>()({
  ok: true, value: { services: [{ id: "api-id", name: "api" }, { id: "worker-id", name: "worker" }] },
}));

const showMore = vi.fn();
vi.spyOn(deploymentCollections, "useDeploymentList").mockReturnValue(asTestDouble<ReturnType<typeof deploymentCollections.useDeploymentList>>()({
  attempts: [
    { deployment: { id: "aaaa2222-uuid", status: "queued", message: "Update nginx", createdAt: new Date() }, view: { status: "queued", deployed: 0, changed: 1, nodes: [] } },
    { deployment: { id: "aaaa1111-uuid", message: "Bump api", createdAt: new Date() }, view: { status: "failed", deployed: 1, changed: 2, nodes: [] } },
  ],
  hasMore: true, loadingMore: false, showMore,
}));
// With its build tail, the queued attempt turns out to be building its images.
vi.spyOn(deploymentCollections, "useDeploymentAttempt").mockReturnValue(asTestDouble<ReturnType<typeof deploymentCollections.useDeploymentAttempt>>()({
  attempt: { view: { status: "building", deployed: 0, changed: 1, nodes: [] } }, pending: false,
}));
vi.spyOn(history, "useNodeDeployments").mockReturnValue(asTestDouble<ReturnType<typeof history.useNodeDeployments>>()({
  data: { pages: [{ running: null, next: null, items: [{ id: "aaaa1111-uuid", message: "Bump api", createdAt: new Date(), outcome: "deployed" }] }] },
  hasNextPage: false, isFetchingNextPage: false, fetchNextPage: vi.fn(),
}));
vi.spyOn(navigation, "useEnvironmentNavigationNodes").mockReturnValue(asTestDouble<ReturnType<typeof navigation.useEnvironmentNavigationNodes>>()({
  nodes: [{ id: "api", name: "api", type: "service" }, { id: "data", name: "data", type: "volume" }],
}));

afterEach(() => { cleanup(); showMore.mockClear(); });

function open(url: string, List = LegacyDeploymentsList) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({
    getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", loader: () => ({ environmentId: "env-1", store: { project: "shop", environment: "production" } }),
    component: () => <InspectorPresentation value={{ takeover: false, toggleFullscreen: () => {}, returnTo: null }}><Outlet /></InspectorPresentation>,
  });
  const list = createRoute({
    getParentRoute: () => environment, path: "deployments",
    validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ service: Schema.optional(Schema.String) })),
    component: function Shown() { return <List service={useSearch({ strict: false }).service ?? null} />; },
  });
  const page = createRoute({ getParentRoute: () => environment, path: "deployments/$deploymentId", component: () => <p>Deployment Page</p> });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([list, page])])])])]),
    history: createMemoryHistory({ initialEntries: [url] }),
  });
  render(<RouterProvider router={router} />);
  return router;
}

it("lists every attempt newest first, active ones read like their page, reads the next page, and opens a row's page with Back returning", async () => {
  const router = open("/cloud/acme/shop/production/deployments");
  const rows = within(await screen.findByRole("navigation", { name: "Deployments" })).getAllByRole("link");
  expect(rows.map((row) => row.textContent)).toEqual([
    expect.stringMatching(/^Update nginxBuilding · aaaa2222/),
    expect.stringMatching(/^Bump apiFailed · 1 of 2 deployed · aaaa1111/),
  ]);
  fireEvent.click(screen.getByRole("button", { name: "Show more" }));
  expect(showMore).toHaveBeenCalledOnce();

  fireEvent.click(rows[1] ?? document.body);
  await screen.findByText("Deployment Page");
  expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/deployments/aaaa1111-uuid");
  router.history.back();
  await screen.findByRole("navigation", { name: "Deployments" });
});

it("narrows to one service's attempts, each showing its outcome and opening the page on that service", async () => {
  open("/cloud/acme/shop/production/deployments?service=api");
  const rows = within(await screen.findByRole("navigation", { name: "Deployments" })).getAllByRole("link");
  expect(rows.map((row) => row.textContent)).toEqual([expect.stringMatching(/^Bump apiDeployed · aaaa1111/)]);
  expect(rows[0]?.getAttribute("href")).toBe("/cloud/acme/shop/production/deployments/aaaa1111-uuid?service=api");
  expect(screen.getByRole("combobox", { name: "Service" }).textContent).toMatch(/^api/);
});

it("over the Store, lists the CLI's and the dashboard's Deployments, narrowed to those that deployed a Service", async () => {
  open("/cloud/acme/shop/production/deployments", StoreDeploymentsList);
  const all = within(await screen.findByRole("navigation", { name: "Deployments" })).getAllByRole("link");
  expect(all.map((row) => row.textContent)).toEqual([
    "Deployment #3Deploying · Deploys worker",
    "Deployment #2Deployed · Deploys every service",
    "Deployment #1Failed · Uploaded by nick · abc1234",
  ]);
  cleanup();

  open("/cloud/acme/shop/production/deployments?service=api-id", StoreDeploymentsList);
  const api = within(await screen.findByRole("navigation", { name: "Deployments" })).getAllByRole("link");
  expect(api.map((row) => row.textContent)).toEqual([expect.stringMatching(/^Deployment #2/), expect.stringMatching(/^Deployment #1/)]);
  expect(api[1]?.getAttribute("href")).toBe("/cloud/acme/shop/production/deployments/d1?service=api-id");
});
