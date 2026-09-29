// @vitest-environment jsdom

import { cleanup, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider, useSearch,
} from "@tanstack/react-router";
import { Schema } from "effect";
import { afterEach, expect, it, vi } from "vitest";
import { InspectorPresentation } from "./CanvasInspectorHeader";
import { DeploymentsList } from "./DeploymentsList";
import { asTestDouble } from "#/lib/test-double";
import * as storeViews from "#/modules/config-store/store-view.queries";
// A CLI Deploy that targeted worker, a full one from the dashboard, and a failed upload.
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

afterEach(() => { cleanup(); });

function open(url: string) {
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
    component: function Shown() { return <DeploymentsList service={useSearch({ strict: false }).service ?? null} />; },
  });
  const page = createRoute({ getParentRoute: () => environment, path: "deployments/$deploymentId", component: () => <p>Deployment Page</p> });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([list, page])])])])]),
    history: createMemoryHistory({ initialEntries: [url] }),
  });
  render(<RouterProvider router={router} />);
  return router;
}

it("lists the CLI's and the dashboard's Deployments, narrowed to those that deployed a Service", async () => {
  open("/cloud/acme/shop/production/deployments");
  const all = within(await screen.findByRole("navigation", { name: "Deployments" })).getAllByRole("link");
  expect(all.map((row) => row.textContent)).toEqual([
    "Deployment #3Deploying · Deploys worker",
    "Deployment #2Deployed · Deploys every service",
    "Deployment #1Failed · Uploaded by nick · abc1234",
  ]);
  cleanup();

  open("/cloud/acme/shop/production/deployments?service=api-id");
  const api = within(await screen.findByRole("navigation", { name: "Deployments" })).getAllByRole("link");
  expect(api.map((row) => row.textContent)).toEqual([expect.stringMatching(/^Deployment #2/), expect.stringMatching(/^Deployment #1/)]);
  expect(api[1]?.getAttribute("href")).toBe("/cloud/acme/shop/production/deployments/d1?service=api-id");
});
