// @vitest-environment jsdom

import { useState } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { DeploymentSummary, Included } from "@ployz/sdk";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { asTestDouble } from "#/lib/test-double";
import { BottomBar, BottomBarSlot } from "./BottomBar";

const [running, queued] = ["aaaa1111-uuid", "aaaa2222-uuid"];
const deployment = (id: string, status: DeploymentSummary["status"], number: number) =>
  asTestDouble<DeploymentSummary>()({ id, status, number, services: [], upload: null });
const replicas: ChangeGroup = asTestDouble<ChangeGroup>()({
  nodeType: "service", nodeId: "api", nodeName: "api", canDiscard: true, changeCount: 1, lifecycle: "update",
  rows: [{ label: "Replicas", currentValue: "1", newValue: "2", kind: "update", path: "api.replicas", changeKey: "api", canDiscard: true }],
});
const cache = asTestDouble<ChangeGroup>()({ ...replicas, nodeId: "cache", nodeName: "cache" });
const onDeploy = vi.fn();
const onDiscardAll = vi.fn();
const onPublish = vi.fn();
/** In flight, newest first, as the Store lists them. */
let active: DeploymentSummary[] = [];
let included: Included[] = [];
const offer = (number: number) =>
  asTestDouble<Included>()({ offered: true, source: { kind: "pull_request", number } });

beforeEach(() => {
  active = [];
  included = [];
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); onDeploy.mockClear(); onPublish.mockReset(); });

function open(url: string, groups: ChangeGroup[] = [], totalChanges = 0, canPublish = false) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", component: Outlet });
  const canvas = createRoute({ getParentRoute: () => environment, id: "_canvas", component: function Canvas() {
    const [slot, setSlot] = useState<HTMLElement | null>(null);
    return (
      <BottomBarSlot.Provider value={slot}>
        <div ref={setSlot} />
        <BottomBar groups={groups} totalChanges={totalChanges} canPublish={canPublish} onDeploy={onDeploy} onPublish={onPublish}
          onDiscardAll={onDiscardAll} onDiscardNode={() => {}} onDiscardRow={() => {}} active={active} notes={{}}
          proposals={{ included }} />
        <Outlet />
      </BottomBarSlot.Provider>
    );
  } });
  const index = createRoute({ getParentRoute: () => canvas, path: "/", component: () => null });
  const page = createRoute({ getParentRoute: () => canvas, path: "deployments/$deploymentId", component: () => <p>Deployment Page</p> });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([
      environment.addChildren([canvas.addChildren([index, page])])])])])]),
    history: createMemoryHistory({ initialEntries: [url] }),
  });
  render(<RouterProvider router={router} />);
  return router;
}
const canvasUrl = "/cloud/acme/shop/production";
const bar = async () => within(await screen.findByRole("group", { name: "Bottom bar" }));

it("shows changes to deploy first, in one row like Railway's: the count, Details, Deploy (⇧+Enter) and Discard under ⋮", async () => {
  open(canvasUrl, [replicas], 1);
  const staged = await bar();
  expect(staged.getAllByText("1 change")).toHaveLength(2);
  // What changed is Details' and the canvas's, so the bar stays small.
  expect(staged.queryByText("api")).toBeNull();
  expect(staged.queryByText(/Replicas/)).toBeNull();
  const deployButton = staged.getByRole("button", { name: /^Deploy/ });
  expect(deployButton.textContent).toBe("Deploy");
  expect(deployButton.getAttribute("aria-keyshortcuts")).toBe("Shift+Enter");

  fireEvent.click(staged.getByRole("button", { name: "Details" }));
  expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));

  await act(async () => { fireEvent.keyDown(document.body, { key: "Enter", shiftKey: true }); });
  expect(onDeploy).toHaveBeenCalledOnce();
  fireEvent.click(staged.getByRole("button", { name: /^Deploy/ }));
  expect(onDeploy).toHaveBeenCalledTimes(2);

  await act(async () => { fireEvent.click(staged.getByRole("button", { name: "More change actions" })); });
  fireEvent.click(await screen.findByRole("menuitem", { name: "Discard all changes" }));
  expect(onDiscardAll).toHaveBeenCalledOnce();
});

it("keeps the review open through Discard all so its inverse draft stays reviewable", async () => {
  onDiscardAll.mockClear();
  open(canvasUrl, [replicas], 1);
  fireEvent.click((await bar()).getByRole("button", { name: "Details" }));
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Discard all" })); });
  expect(onDiscardAll).toHaveBeenCalledOnce();
  expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
});

it("keeps staged changes over a running Deployment, with Deploy next", async () => {
  active = [deployment(running, "running", 1)];
  open(canvasUrl, [replicas, cache], 2);
  const staged = await bar();
  expect(staged.getAllByText("2 changes")).toHaveLength(2);
  expect(staged.getByRole("button", { name: /^Deploy next/ })).toBeTruthy();
  expect(staged.queryByRole("link", { name: "Logs" })).toBeNull();
});

it("otherwise shows the running Deployment, and the queued one while the running one's page is open", async () => {
  active = [deployment(queued, "queued", 2), deployment(running, "running", 1)];
  const router = open(canvasUrl);
  const shown = await bar();
  expect(shown.getByText("Deploying · Deployment #1")).toBeTruthy();
  expect(shown.getByText("Deploys every service")).toBeTruthy();
  fireEvent.click(shown.getByRole("link", { name: "Logs" }));
  await screen.findByText("Deployment Page");
  expect(router.state.location.href).toBe(`${canvasUrl}/deployments/${running}`);
  expect((await bar()).getByText("Queued · Deployment #2")).toBeTruthy();
});

it("keeps the offers in view under a queued Deployment", async () => {
  active = [deployment(queued, "queued", 2)];
  included = [offer(5), offer(6)];
  open(canvasUrl);
  const shown = await bar();
  expect(shown.getByText("Queued · Deployment #2")).toBeTruthy();
  expect(shown.getByText("PR #5, PR #6")).toBeTruthy();
  expect(shown.getByText("Queued")).toBeTruthy();
});

it("hides while nothing is staged or running, and while the only Deployment's page is open", async () => {
  active = [deployment(running, "running", 1)];
  open(`${canvasUrl}/deployments/${running}`);
  await screen.findByText("Deployment Page");
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
  cleanup();
  active = [];
  open(canvasUrl);
  await act(async () => {});
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});


it("preserves the Change message while a Save can still refuse or fail", async () => {
  open(canvasUrl, [replicas], 1, true);
  fireEvent.click((await bar()).getByRole("button", { name: "Details" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Change message" }), { target: { value: "Explain this change" } });
  await act(async () => { fireEvent.click(screen.getByRole("button", { name: "Save" })); });
  expect(onPublish).toHaveBeenCalledWith("Explain this change");
  expect(screen.getByRole("textbox", { name: "Change message" })).toHaveProperty("value", "Explain this change");
  expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
});
