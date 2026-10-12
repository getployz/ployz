// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import type { MachineId } from "@ployz/sdk";
import { mirrorPanel, type MirrorPanel, type PlacementRow } from "#/modules/volume-placement/volume-placement";
import { COPIES, member } from "#/modules/volume-run/volume-run.test-fixture";
import type { Member } from "#/modules/volume-run/volume-run";
import { InspectorPresentation } from "../../../-components/CanvasInspectorHeader";
import { MirrorDrawerView } from "./StoreMirrorDrawer";

const volume = { id: "v1", name: "pg-data" };
const id = (server: string) => server.padEnd(32, "0") as MachineId;
const servers = ["web-1", "web-2", "web-3"].map(id);
const row = (mirrors: string[], staged: { primary: string; mirrors?: string[]; deletes?: string[] } | null = null): PlacementRow => ({
  volumeId: "v1",
  environmentId: "e1",
  mirrors: mirrors.map(id),
  staged: staged === null ? null : { primary: id(staged.primary), mirrors: (staged.mirrors ?? []).map(id), deletes: (staged.deletes ?? []).map(id) },
  deploymentId: null,
});
const fourMinutesAgo = (server: string): Member => {
  const base = member(server, "mirror");
  if (!base.answered || base.view.copy === null) throw new Error("fixture");
  const newest = { name: "s", guid: "12", created_unix_seconds: (Date.now() - 4 * 60_000) / 1000 };
  return { ...base, view: { ...base.view, copy: { ...COPIES.mirror, newest } } };
};
const live = [member("web-1", "writer"), fourMinutesAgo("web-2"), member("web-3", "empty", { pool: true })];

afterEach(cleanup);

async function show(panel: MirrorPanel | null) {
  if (panel === null) throw new Error("no mirror");
  const stage = vi.fn();
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", component: Outlet });
  const resource = createRoute({ getParentRoute: () => environment, path: "resources/$resourceId", component: () => (
    <InspectorPresentation value={{ takeover: false, toggleFullscreen() {}, returnTo: null }}>
      <MirrorDrawerView params={{ organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production" }}
        panel={panel} disabled={false} stage={stage} />
    </InspectorPresentation>
  ) });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([resource])])])])]),
    history: createMemoryHistory({ initialEntries: [`/cloud/acme/shop/production/resources/v1~${id("web-2")}`] }),
  });
  render(<RouterProvider router={router} />);
  await screen.findByText("pg-data-web-2");
  return { stage, router };
}

/** Each settings row as `title: value`. */
const rows = () => [...document.querySelectorAll("[data-slot=field]")].map((field) => field.textContent);
const header = () => screen.getByText("pg-data-web-2").closest(".canvas-inspector-header") as HTMLElement;

it("names a live mirror, links its Volume, and says where and how fresh its copy is", async () => {
  const { router } = await show(mirrorPanel(volume, live, row(["web-2"]), id("web-2"), servers));
  expect(within(header()).getByRole("link", { name: "pg-data" })).toBeTruthy();
  expect(within(header()).getByText(/Mirror of/)).toBeTruthy();
  expect(rows()).toEqual(expect.arrayContaining([
    "Serverweb-2", "Last refresh4 minutes ago", "RefreshesOn every deploy and every 15 minutes", "Restore pointsIn pg-data",
  ]));
  fireEvent.click(screen.getByRole("link", { name: "In pg-data" }));
  await screen.findByText("pg-data-web-2");
  expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/resources/v1");
});

it("stages the same move the Volume's Primary select stages", async () => {
  const { stage } = await show(mirrorPanel(volume, live, row(["web-2"]), id("web-2"), servers));
  fireEvent.click(screen.getByRole("button", { name: "Make primary" }));
  expect(stage).toHaveBeenLastCalledWith({ primary: id("web-2"), mirrors: [], deletes: [] }, [id("web-2")]);
});

it("stages its removal, which deletes its copy on the next Deploy", async () => {
  const { stage } = await show(mirrorPanel(volume, live, row(["web-2"]), id("web-2"), servers));
  expect(screen.getByText("Its copy on web-2 is deleted on next deploy.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Remove mirror" }));
  expect(stage).toHaveBeenLastCalledWith({ primary: id("web-1"), mirrors: [], deletes: [] }, [id("web-2")]);
});

it("shows a staged mirror as New, with nothing refreshed and nothing to do but discard in Details", async () => {
  const added = [member("web-1", "writer"), member("web-2", "empty", { pool: true })];
  await show(mirrorPanel(volume, added, row([], { primary: "web-1", mirrors: ["web-2"] }), id("web-2"), servers));
  expect(within(header()).getByText("New")).toBeTruthy();
  expect(rows()).toContain("Last refreshNot yet");
  expect(screen.queryByRole("button", { name: "Make primary" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Remove mirror" })).toBeNull();
});

it("shows a mirror staged to take over as Primary, with nothing to do", async () => {
  await show(mirrorPanel(volume, live, row(["web-2"], { primary: "web-2" }), id("web-2"), servers));
  expect(within(header()).getByText("Primary")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Make primary" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Remove mirror" })).toBeNull();
});

it("strikes a mirror staged for removal", async () => {
  await show(mirrorPanel(volume, live, row(["web-2"], { primary: "web-1" }), id("web-2"), servers));
  expect(within(header()).getByText("Removing")).toBeTruthy();
  expect(screen.getByText("pg-data-web-2").className).toMatch(/line-through/);
});
