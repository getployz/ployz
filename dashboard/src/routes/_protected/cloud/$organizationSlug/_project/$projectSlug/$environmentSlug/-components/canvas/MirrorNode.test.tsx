// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, expect, it } from "vitest";
import type { MachineId, VolumeListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import type { CanvasMirror } from "#/modules/volume-placement/volume-placement";
import { CanvasNodeList } from "./CanvasServiceList";

const volume = asTestDouble<VolumeListing>()({ id: "vol-1", name: "data", mounts: [], deployed: true, storage: { kind: "docker" }, storage_locked: false, change: null });
const mirror = (change: CanvasMirror["change"], newest: Date | null = new Date(Date.now() - 4 * 60_000)): CanvasMirror =>
  ({ volume, server: { id: "m2" as MachineId, name: "web-2" }, change, newest });

afterEach(cleanup);

async function show(mirrors: readonly CanvasMirror[]) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", component: () => (
    <CanvasNodeList store={{ services: [], unmountedVolumes: [volume], unmountedConfigs: [], mirrors: new Map([[volume.id, mirrors]]) }}
      selectedNodeId={null} />
  ) });
  const resource = createRoute({ getParentRoute: () => environment, path: "resources/$resourceId", component: () => null });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([resource])])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }),
  });
  render(<RouterProvider router={router} />);
  await screen.findByText("data");
  return router;
}

const node = () => screen.getByRole("link", { name: "data-web-2, mirror of data" });

it("shows a live mirror by name with how fresh it is, and opens its Volume", async () => {
  const router = await show([mirror(null)]);
  expect(node().textContent).toBe("data-web-2Mirror · 4 minutes ago");
  expect(node().className).not.toMatch(/soft/);
  fireEvent.click(node());
  await screen.findByText("data");
  expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/resources/vol-1");
});

it("shows a staged mirror green with New", async () => {
  await show([mirror("add", null)]);
  expect(within(node()).getByText("New")).toBeTruthy();
  expect(node().className).toMatch(/bg-success-soft/);
});

it("shows a staged removal red and struck through with Removing", async () => {
  await show([mirror("remove")]);
  expect(within(node()).getByText("Removing")).toBeTruthy();
  expect(node().className).toMatch(/bg-destructive-soft/);
  expect(within(node()).getByText("data-web-2").className).toMatch(/line-through/);
});

it("shows no mirror node for a Volume without mirrors", async () => {
  await show([]);
  expect(screen.queryByRole("link", { name: /mirror of/ })).toBeNull();
});
