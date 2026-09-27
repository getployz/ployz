// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { Schema } from "effect";
import { afterEach, expect, it, vi } from "vitest";
import { InspectorPresentation } from "./CanvasInspectorHeader";
import { DeploymentsList } from "./DeploymentsList";

const showMore = vi.fn();
vi.mock("#/modules/deployments/deployment.collection", () => ({
  useDeploymentList: () => ({
    attempts: [
      { deployment: { id: "aaaa2222-uuid", message: "Update nginx", createdAt: new Date() }, view: { status: "queued", deployed: 0, changed: 1, nodes: [] } },
      { deployment: { id: "aaaa1111-uuid", message: "Bump api", createdAt: new Date() }, view: { status: "failed", deployed: 1, changed: 2, nodes: [] } },
    ],
    hasMore: true, loadingMore: false, showMore,
  }),
}));
vi.mock("#/modules/deployments/deployment-history.queries", () => ({
  useNodeDeployments: () => ({
    data: { pages: [{ running: null, next: null, items: [{ id: "aaaa1111-uuid", message: "Bump api", createdAt: new Date(), outcome: "deployed" }] }] },
    hasNextPage: false, isFetchingNextPage: false, fetchNextPage: vi.fn(),
  }),
}));
vi.mock("./environment-node-navigation", () => ({
  useEnvironmentNavigationNodes: () => ({ nodes: [{ id: "api", name: "api", type: "service" }, { id: "data", name: "data", type: "volume" }] }),
}));

afterEach(() => { cleanup(); vi.clearAllMocks(); });

function open(url: string) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({
    getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", loader: () => ({ environmentId: "env-1" }),
    component: () => <InspectorPresentation value={{ takeover: false, toggleFullscreen: () => {} }}><Outlet /></InspectorPresentation>,
  });
  const list = createRoute({
    getParentRoute: () => environment, path: "deployments",
    validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ service: Schema.optional(Schema.String) })),
    component: function List() { return <DeploymentsList service={list.useSearch().service ?? null} />; },
  });
  const page = createRoute({ getParentRoute: () => environment, path: "deployments/$deploymentId", component: () => <p>Deployment Page</p> });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([list, page])])])])]),
    history: createMemoryHistory({ initialEntries: [url] }),
  });
  render(<RouterProvider router={router} />);
  return router;
}

it("lists every attempt newest first, reads the next page, and opens a row's page with Back returning", async () => {
  const router = open("/cloud/acme/shop/production/deployments");
  const rows = within(await screen.findByRole("navigation", { name: "Deployments" })).getAllByRole("link");
  expect(rows.map((row) => row.textContent)).toEqual([
    expect.stringMatching(/^Update nginxQueued · aaaa2222/),
    expect.stringMatching(/^Bump apiFailed · 1 of 2 deployed · aaaa1111/),
  ]);
  fireEvent.click(screen.getByRole("button", { name: "Show more" }));
  expect(showMore).toHaveBeenCalledOnce();

  fireEvent.click(rows[1]!);
  await screen.findByText("Deployment Page");
  expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/deployments/aaaa1111-uuid");
  router.history.back();
  await screen.findByRole("navigation", { name: "Deployments" });
});

it("narrows to one service's attempts, each showing its outcome and opening the page on that service", async () => {
  open("/cloud/acme/shop/production/deployments?service=api");
  const rows = within(await screen.findByRole("navigation", { name: "Deployments" })).getAllByRole("link");
  expect(rows.map((row) => row.textContent)).toEqual([expect.stringMatching(/^Bump apiDeployed · aaaa1111/)]);
  expect(rows[0]!.getAttribute("href")).toBe("/cloud/acme/shop/production/deployments/aaaa1111-uuid?service=api");
  expect(screen.getByRole("combobox", { name: "Service" }).textContent).toMatch(/^api/);
});
