// @vitest-environment jsdom

import { useState } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as branchCollections from "#/modules/branches/branch.collection";
import * as deploymentCollections from "#/modules/deployments/deployment.collection";
import * as branchReviews from "#/modules/branches/use-branch-review";
import * as conditionalSaves from "#/modules/pr-environments/conditional-save.collection";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import type { ChangeRow } from "#/modules/branches/branch-review";
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
let attempts: ReturnType<typeof attempt>[] = [];
let startingPoint: { name: string } | undefined;
let branch: branchReviews.BranchReviewView | null = null;
let held: ConditionalSaveRow[] = [];
const imageRow = (from: string): ChangeRow => ({ key: `web-lineage:source.image`, role: "move", conflict: false, base: "web:1", from, into: "web:1" });
const branchReview = (merge: ChangeRow[], updates: number) => asTestDouble<branchReviews.BranchReviewView>()({
  parent: { id: "env-0", name: "production", namespace: "shop-production" },
  merge, changes: merge.length, updates, nameOf: () => "web",
});

beforeEach(() => {
  attempts = [];
  startingPoint = undefined;
  branch = null;
  held = [];
  vi.spyOn(conditionalSaves, "useHeldChanges").mockImplementation(() => held);
  vi.spyOn(conditionalSaves, "useStagedInstead").mockImplementation(() => []);
  vi.spyOn(branchCollections, "useStartingPoint").mockImplementation(() =>
    asTestDouble<ReturnType<typeof branchCollections.useStartingPoint>>()(startingPoint));
  vi.spyOn(branchReviews, "useBranchReview").mockImplementation(() => branch);
  vi.spyOn(branchCollections, "useHasStagedChanges").mockImplementation(() => false);
  vi.spyOn(deploymentCollections, "useEnvironmentDeployments").mockImplementation(() =>
    asTestDouble<ReturnType<typeof deploymentCollections.useEnvironmentDeployments>>()(attempts));
  vi.spyOn(deploymentCollections, "useDeploymentAttempt").mockImplementation((_organization, _environment, id) =>
    asTestDouble<ReturnType<typeof deploymentCollections.useDeploymentAttempt>>()({
      attempt: attempts.find((candidate) => candidate.deployment.id === id), pending: false,
    }));
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
  const newBranch = createRoute({ getParentRoute: () => canvas, path: "new-branch", component: () => <p>New branch panel</p> });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([
      environment.addChildren([canvas.addChildren([index, page, newBranch])])])])])]),
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

it("shows a starting point's state over its staged nodes, with New branch and no Deploy", async () => {
  startingPoint = { name: "template" };
  const router = open(canvasUrl, [replicas, cache], 2);
  const shown = await bar();
  expect(shown.getByText("template isn't deployed")).toBeTruthy();
  expect(shown.getByText("A starting point for branches")).toBeTruthy();
  expect(shown.queryByText("2 changes")).toBeNull();
  expect(shown.queryByRole("button", { name: /^Deploy/ })).toBeNull();
  await act(async () => { fireEvent.keyDown(document.body, { key: "Enter", shiftKey: true }); });
  expect(onDeploy).not.toHaveBeenCalled();
  fireEvent.click(shown.getByRole("link", { name: "New branch" }));
  await screen.findByText("New branch panel");
  expect(router.state.location.href).toBe(`${canvasUrl}/new-branch`);
});

it("on a Branch, then shows what would merge into its Parent, else what's new there, after the staged and running states", async () => {
  branch = branchReview([imageRow("web:2"), imageRow("web:3")], 1);
  attempts = [attempt(running, "deploying", "Add worker")];
  open(canvasUrl, [replicas], 1);
  // Staged changes come first, and Review opens the Branch's review page.
  expect((await bar()).getByRole("link", { name: "Review" }).getAttribute("href")).toBe(`${canvasUrl}/review`);
  cleanup();
  open(canvasUrl);
  expect((await bar()).getByText("Deploying · Add worker")).toBeTruthy();
  cleanup();
  attempts = [];
  open(canvasUrl);
  const changes = await bar();
  expect(changes.getByText("2 changes for production")).toBeTruthy();
  expect(changes.getByText("web · Container image web:1 → web:2")).toBeTruthy();
  expect(changes.getByRole("link", { name: "Review and merge" }).getAttribute("href")).toBe(`${canvasUrl}/review`);
  cleanup();
  branch = branchReview([], 1);
  open(canvasUrl);
  const updates = await bar();
  expect(updates.getByText("1 update from production")).toBeTruthy();
  expect(updates.getByRole("link", { name: "Review" })).toBeTruthy();
  cleanup();
  branch = branchReview([], 0);
  open(canvasUrl);
  await act(async () => {});
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});

it("on a PR Environment, says whether it's approved; on a Destination, what waits for a pull request once nothing else shows", async () => {
  const pullRequest = (check: NonNullable<branchReviews.BranchReviewView["check"]>) => asTestDouble<branchReviews.BranchReviewView>()({
    ...branchReview([], 0),
    pullRequest: { number: 142, title: "Discounts", author: "maya", headBranch: "discounts", targetBranch: "main", commits: 2, closed: false },
    goesTo: [{ destination: { id: "env-0", name: "production", namespace: "shop-production" }, rows: [imageRow("web:2")], review: "r", approval: null }],
    changes: 1, check,
  });
  branch = pullRequest({ passing: false, reason: "Review and approve 1 change for production" });
  open(canvasUrl);
  const unapproved = await bar();
  expect(unapproved.getByText("1 change for production")).toBeTruthy();
  expect(unapproved.getByText("Review and approve 1 change for production")).toBeTruthy();
  expect(unapproved.getByRole("link", { name: "Review and approve" })).toBeTruthy();
  cleanup();
  branch = pullRequest({ passing: true, reason: "1 change approved for production by maya" });
  open(canvasUrl);
  const approved = await bar();
  expect(approved.getByText("Approved")).toBeTruthy();
  expect(approved.getByText("Lands when #142 merges")).toBeTruthy();
  expect(approved.getByRole("link", { name: "Review" })).toBeTruthy();
  cleanup();
  // Approved, but the check still wants something: the bar says what.
  branch = pullRequest({ passing: false, reason: "STRIPE_KEY needs a value for production" });
  open(canvasUrl);
  expect((await bar()).getByText("STRIPE_KEY needs a value for production")).toBeTruthy();
  cleanup();

  held = [asTestDouble<ConditionalSaveRow>()({ id: "save", prNumber: 142, approvedBy: "maya", rows: [{ row: imageRow("web:2"), missing: false }] })];
  branch = branchReview([imageRow("web:2")], 0);
  open(canvasUrl);
  expect((await bar()).getByText("1 change for production")).toBeTruthy();
  cleanup();
  branch = null;
  open(canvasUrl);
  const waiting = await bar();
  expect(waiting.getByText("Waiting for #142")).toBeTruthy();
  expect(waiting.getByText("1 change · approved by maya")).toBeTruthy();
  expect(waiting.getByRole("button", { name: "Review" })).toBeTruthy();
});
