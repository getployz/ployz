// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, expect, it } from "vitest";
import type { MachineId, VolumeListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import type { MirrorCopy, OffsiteCopy, OldCopy, VolumeCopies } from "#/modules/volume-copies/volume-copies";
import { CanvasNodeList } from "./CanvasServiceList";

const volume = asTestDouble<VolumeListing>()({ id: "vol-1", name: "data", mounts: [], deployed: true, storage: { kind: "docker" }, storage_locked: false, change: null });
const minutesAgo = (minutes: number) => new Date(Date.now() - minutes * 60_000);
const mirror = (extra: Partial<MirrorCopy> = {}): MirrorCopy => ({
  kind: "mirror", id: "m2", server: { id: "m2" as MachineId, name: "web-2" }, name: "data-web-2", refreshedAt: minutesAgo(4),
  skipped: null, staged: null, busy: null, ...extra,
});
const offsite: OffsiteCopy = {
  kind: "offsite", id: "acme-backups", bucket: "acme-backups", region: "eu-central-1", keep: 7, uploadedAt: minutesAgo(10), staged: null, busy: null,
};
const old: OldCopy = {
  kind: "old", id: "m3", server: { id: "m3" as MachineId, name: "web-3" }, name: "data-old", since: null, bytes: 1e6, staged: null, busy: null,
};
const copiesOf = (elsewhere: VolumeCopies["elsewhere"]): VolumeCopies => ({
  volume: { id: volume.id, name: volume.name },
  live: { server: { id: "m1" as MachineId, name: "web-1" }, restore: null, moveTo: null, busy: null, snapshots: null },
  elsewhere,
});

afterEach(cleanup);

async function show(elsewhere: VolumeCopies["elsewhere"]) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", loader: () => ({ store: { project: "shop", environment: "production" } }), component: () => (
    <CanvasNodeList store={{ services: [], unmountedVolumes: [volume], unmountedConfigs: [], copies: new Map([[volume.id, copiesOf(elsewhere)]]) }}
      selectedNodeId={null} />
  ) });
  const resource = createRoute({ getParentRoute: () => environment, path: "resources/$resourceId", component: () => null });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([resource])])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }),
  });
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByText("data");
  return router;
}

const node = () => screen.getByRole("link", { name: "data-web-2, mirror of data" });

it("shows a live mirror by name with how fresh it is, and opens its own drawer", async () => {
  const router = await show([mirror()]);
  expect(node().textContent).toBe("data-web-2Mirror · 4 minutes ago");
  expect(node().className).not.toMatch(/soft/);
  fireEvent.click(node());
  await screen.findByText("data");
  expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/resources/vol-1~m2");
});

it("warns in amber when a mirror has fallen behind", async () => {
  await show([mirror({ refreshedAt: minutesAgo(3 * 60) })]);
  const behind = within(node()).getByLabelText("Behind");
  expect(behind.parentElement?.className).toMatch(/text-warning/);
  expect(behind.parentElement?.textContent).toBe("3 hours ago");
});

it("shows a busy mirror with a spinner and what it is doing", async () => {
  await show([mirror({ busy: "Copying 40%" })]);
  expect(node().textContent).toBe("data-web-2Copying 40%");
  expect(node().querySelector("[aria-hidden]")).not.toBeNull();
});

it("shows a staged mirror green with New", async () => {
  await show([mirror({ staged: "new", refreshedAt: null })]);
  expect(within(node()).getByText("New")).toBeTruthy();
  expect(node().className).toMatch(/bg-success-soft/);
});

it("shows a staged removal red and struck through with Removing", async () => {
  await show([mirror({ staged: "removing" })]);
  expect(node().textContent).toBe("data-web-2Removing");
  expect(node().className).toMatch(/bg-destructive-soft/);
  expect(within(node()).getByText("data-web-2").className).toMatch(/line-through/);
});

it("shows a mirror staged to become the Primary blue, with Primary", async () => {
  await show([mirror({ staged: "primary" })]);
  expect(within(node()).getByText("Primary")).toBeTruthy();
  expect(node().className).toMatch(/bg-info-soft/);
});

it("hangs the offsite copy beside the mirror on one bar, and never the old copy", async () => {
  await show([mirror(), offsite, old]);
  const cards = [...document.querySelectorAll("[data-copy]")].map((card) => card.getAttribute("data-copy"));
  expect(cards).toEqual(["data-web-2", "acme-backups"]);
  expect(document.querySelector("[data-copy=acme-backups]")?.textContent).toBe("acme-backupsOffsite · 10 minutes ago");
  expect(node().parentElement?.querySelector("i")).not.toBeNull();
});

it("draws a single card without a bar", async () => {
  await show([mirror()]);
  expect(node().parentElement?.querySelector("i")).toBeNull();
});

it("hangs nothing under a Volume whose only other copy is old", async () => {
  await show([old]);
  expect(document.querySelector("[data-copy]")).toBeNull();
});
