// @vitest-environment jsdom

import { useState } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as deploymentCollections from "#/modules/deployments/deployment.collection";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { asTestDouble } from "#/lib/test-double";
import { BottomBar, BottomBarSlot } from "./BottomBar";

const [running, queued] = ["aaaa1111-uuid", "aaaa2222-uuid"];
const attempt = (id: string, status: string, message: string, origin = "manual") => ({
  deployment: { id, status, message, runtimeProgress: null, triggerOrigin: { origin } },
  nodes: [{ nodeId: "api", name: "api" }],
  view: { status, deployed: 0, changed: 1, nodes: [{ nodeId: "api", outcome: status }] },
});
const replicas: CanvasEnvironmentChangeGroup = asTestDouble<CanvasEnvironmentChangeGroup>()({
  nodeType: "service", nodeId: "api", nodeName: "api", canDiscard: true, changeCount: 1,
  rows: [{ label: "Replicas", currentValue: "1", newValue: "2" }],
});
const cache = asTestDouble<CanvasEnvironmentChangeGroup>()({ ...replicas, nodeId: "cache", nodeName: "cache" });
const onDeploy = vi.fn();
const onDiscardAll = vi.fn(async () => true);
let attempts: unknown[] = [];

beforeEach(() => {
  attempts = [];
  vi.spyOn(deploymentCollections, "useEnvironmentDeployments").mockImplementation(() =>
    asTestDouble<ReturnType<typeof deploymentCollections.useEnvironmentDeployments>>()(attempts));
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); onDeploy.mockClear(); });

function open(url: string, groups: CanvasEnvironmentChangeGroup[] = [], totalChanges = 0) {
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
        <BottomBar environmentId="env-1" groups={groups} totalChanges={totalChanges} canDeploy commitMessage=""
          canSaveWithoutDeploying={false} onCommitMessageChange={() => {}} onDeploy={onDeploy} onSaveWithoutDeploying={() => {}}
          onDiscardAll={onDiscardAll} onDiscardNode={() => {}} onDiscardRow={() => {}} />
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

it("shows staged changes first: the count, the one change, Review, Deploy (⇧+Enter) and Discard under ⋮", async () => {
  open(canvasUrl, [replicas], 1);
  const staged = await bar();
  expect(staged.getByText("1 change")).toBeTruthy();
  expect(staged.getByText("api · Replicas 1 → 2")).toBeTruthy();
  expect(staged.getByRole("button", { name: /^Deploy/ }).textContent).toBe("Deploy⇧+Enter");

  fireEvent.click(staged.getByRole("button", { name: "Review" }));
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

it("names the changed services, and keeps staged changes over a running Git-triggered deployment with Deploy next", async () => {
  attempts = [attempt(running, "deploying", "Push to main", "github")];
  open(canvasUrl, [replicas, cache], 2);
  const staged = await bar();
  expect(staged.getByText("2 changes")).toBeTruthy();
  expect(staged.getByText("api, cache")).toBeTruthy();
  expect(staged.getByRole("button", { name: /^Deploy next/ })).toBeTruthy();
  expect(staged.queryByRole("link", { name: "Logs" })).toBeNull();
});

it("otherwise shows the running attempt, and the queued one while the running one's page is open", async () => {
  attempts = [attempt(queued, "queued", "Update nginx"), attempt(running, "deploying", "Add worker")];
  const router = open(canvasUrl);
  const shown = await bar();
  expect(shown.getByText("Deploying · Add worker")).toBeTruthy();
  expect(shown.getByText("api · Deploying")).toBeTruthy();
  fireEvent.click(shown.getByRole("link", { name: "Logs" }));
  await screen.findByText("Deployment Page");
  expect(router.state.location.href).toBe(`${canvasUrl}/deployments/${running}?service=api`);
  expect((await bar()).getByText("Queued · Update nginx")).toBeTruthy();
});

it("hides while nothing is staged or running, and while the only attempt's page is open", async () => {
  attempts = [attempt(running, "deploying", "Add worker")];
  open(`${canvasUrl}/deployments/${running}`);
  await screen.findByText("Deployment Page");
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
  cleanup();
  attempts = [attempt(running, "applied", "Add worker")];
  open(canvasUrl);
  await act(async () => {});
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});
