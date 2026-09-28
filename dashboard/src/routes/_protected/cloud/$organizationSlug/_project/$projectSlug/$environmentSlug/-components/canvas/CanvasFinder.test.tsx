// @vitest-environment jsdom

import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider, useParams,
} from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { CanvasFinder } from "./CanvasFinder";

const nodes = [
  { id: "api", name: "api", type: "service" },
  { id: "data", name: "pg-data", type: "volume" },
] as const;

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView = () => {};
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

function Opened() {
  const { serviceId, resourceId } = useParams({ strict: false });
  return <p>Opened {serviceId ?? resourceId}</p>;
}

async function openCanvas() {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected" });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug" });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project" });
  const environment = createRoute({
    getParentRoute: () => projectGroup,
    path: "$projectSlug/$environmentSlug",
    component: () => <><CanvasFinder nodes={[...nodes]} /><input aria-label="Service name" /><Outlet /></>,
  });
  const canvas = createRoute({ getParentRoute: () => environment, id: "_canvas" });
  const index = createRoute({ getParentRoute: () => canvas, path: "/" });
  const service = createRoute({ getParentRoute: () => canvas, path: "services/$serviceId", component: Opened });
  const resource = createRoute({ getParentRoute: () => canvas, path: "resources/$resourceId", component: Opened });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([
      projectGroup.addChildren([environment.addChildren([canvas.addChildren([index, service, resource])])]),
    ])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }),
  });
  render(<RouterProvider router={router} />);
  await screen.findByRole("button", { name: /Find/ });
  return router;
}

it("opens from the Find button and opens the chosen service's panel", async () => {
  const router = await openCanvas();
  fireEvent.click(screen.getByRole("button", { name: /Find/ }));
  fireEvent.click(await screen.findByRole("option", { name: "api" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/services/api"));
  expect(await screen.findByText("Opened api")).toBeTruthy();
});

it("opens on `/` and opens a chosen volume", async () => {
  const router = await openCanvas();
  act(() => { fireEvent.keyDown(document.body, { key: "/" }); });
  fireEvent.click(await screen.findByRole("option", { name: "pg-data" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/resources/data"));
});

it("never opens on `/` typed into a field", async () => {
  await openCanvas();
  fireEvent.keyDown(screen.getByRole("textbox", { name: "Service name" }), { key: "/" });
  expect(screen.queryByRole("dialog")).toBeNull();
});
