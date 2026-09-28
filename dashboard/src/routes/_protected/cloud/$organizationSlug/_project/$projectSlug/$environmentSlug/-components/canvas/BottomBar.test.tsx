// @vitest-environment jsdom

import { useState } from "react";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as branchCollections from "#/modules/branches/branch.collection";
import * as branchCommands from "#/modules/branches/branch-commands";
import * as documents from "#/modules/environment-design/environment-document.collection";
import * as workspaces from "#/modules/environment-design/workspace.queries";
import * as deploymentCollections from "#/modules/deployments/deployment.collection";
import * as branchReviews from "#/modules/branches/use-branch-review";
import * as conditionalSaves from "#/modules/pr-environments/conditional-save.collection";
import * as saveCommands from "#/modules/pr-environments/conditional-save-commands";
import * as offCommands from "#/modules/pr-environments/off-commands";
import * as lineageNames from "#/modules/branches/use-lineage-names";
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
const update = vi.fn();
const withdraw = vi.fn();
const start = vi.fn();
const shutDown = vi.fn();
let shutdown: "running" | "off" | "failed" | null = null;
let unsettled: string | null = null;
let attempts: ReturnType<typeof attempt>[] = [];
let startingPoint: { name: string } | undefined;
let branch: branchReviews.BranchReviewView | null = null;
let held: ConditionalSaveRow[] = [];
const imageRow = (from: string, conflict = false): ChangeRow => ({ key: `web-lineage:source.image`, role: "move", conflict, base: "web:1", from, into: "web:1" });
const branchReview = (save: ChangeRow[], updates: number) => asTestDouble<branchReviews.BranchReviewView>()({
  parent: { id: "env-0", name: "production", namespace: "shop-production" }, pullRequest: null, kept: false,
  save, saveReview: "r", changes: save.length, update: updates ? [imageRow("web:2")] : [], updates, nameOf: () => "web",
});

beforeEach(() => {
  attempts = [];
  startingPoint = undefined;
  branch = null;
  held = [];
  shutdown = null;
  vi.spyOn(branchCollections, "useShutdown").mockImplementation(() => shutdown);
  vi.spyOn(offCommands, "usePrEnvironmentOff").mockImplementation(() => asTestDouble<ReturnType<typeof offCommands.usePrEnvironmentOff>>()({
    start: { mutate: start, isPending: false }, shutDown: { mutate: shutDown, isPending: false },
  }));
  vi.spyOn(conditionalSaves, "useWaitingSaves").mockImplementation(() => held);
  vi.spyOn(conditionalSaves, "useLandedSaves").mockImplementation(() => []);
  vi.spyOn(saveCommands, "useConditionalSave").mockImplementation(() =>
    asTestDouble<ReturnType<typeof saveCommands.useConditionalSave>>()({ save: { mutate: vi.fn(), isPending: false }, withdraw: { mutate: withdraw, isPending: false } }));
  vi.spyOn(saveCommands, "useTakePullRequestValue").mockImplementation(() =>
    asTestDouble<ReturnType<typeof saveCommands.useTakePullRequestValue>>()({ mutate: vi.fn(), isPending: false }));
  vi.spyOn(lineageNames, "useLineageNames").mockImplementation(() => () => "web");
  vi.spyOn(documents, "useEnvironmentDocuments").mockImplementation(() => []);
  vi.spyOn(branchCollections, "useStartingPoint").mockImplementation(() =>
    asTestDouble<ReturnType<typeof branchCollections.useStartingPoint>>()(startingPoint));
  vi.spyOn(branchReviews, "useBranchReview").mockImplementation(() => branch);
  unsettled = null;
  vi.spyOn(branchCollections, "useBranchUnsettled").mockImplementation(() => unsettled);
  vi.spyOn(branchCommands, "useUpdateBranch").mockImplementation(() => ({ update, makeOwnCopy: vi.fn() }));
  vi.spyOn(branchCommands, "useSaveBranch").mockImplementation(() =>
    asTestDouble<ReturnType<typeof branchCommands.useSaveBranch>>()({ mutate: vi.fn(), isPending: false, isError: false }));
  vi.spyOn(documents, "useEnvironmentDocument").mockImplementation(() =>
    asTestDouble<ReturnType<typeof documents.useEnvironmentDocument>>()({ name: "fix-web", intent: { services: [] } }));
  vi.spyOn(workspaces, "useWorkspace").mockImplementation(() =>
    asTestDouble<ReturnType<typeof workspaces.useWorkspace>>()({ projects: [] }));
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

it("shows changes to deploy first: the count, the one change, Details, Deploy (⇧+Enter) and Discard under ⋮", async () => {
  open(canvasUrl, [replicas], 1);
  const staged = await bar();
  expect(staged.getByText("1 change to deploy")).toBeTruthy();
  expect(staged.getByText("api · Replicas 1 → 2")).toBeTruthy();
  expect(staged.getByRole("button", { name: /^Deploy/ }).textContent).toBe("Deploy⇧+Enter");

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

it("names the changed services, and keeps staged changes over a running Git-triggered deployment with Deploy next", async () => {
  attempts = [attempt(running, "deploying", "Push to main", "github")];
  open(canvasUrl, [replicas, cache], 2);
  const staged = await bar();
  expect(staged.getByText("2 changes to deploy")).toBeTruthy();
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
  expect(shown.queryByText("2 changes to deploy")).toBeNull();
  expect(shown.queryByRole("button", { name: /^Deploy/ })).toBeNull();
  await act(async () => { fireEvent.keyDown(document.body, { key: "Enter", shiftKey: true }); });
  expect(onDeploy).not.toHaveBeenCalled();
  fireEvent.click(shown.getByRole("link", { name: "New branch" }));
  await screen.findByText("New branch panel");
  expect(router.state.location.href).toBe(`${canvasUrl}/new-branch`);
});

it("on a Branch, adds a second row: changes to save with Save beside every first row, else updates with Update", async () => {
  branch = branchReview([imageRow("web:2"), imageRow("web:3", true)], 1);
  attempts = [attempt(running, "deploying", "Add worker")];
  open(canvasUrl, [replicas], 1);
  const staged = await bar();
  expect(staged.getByText("1 change to deploy")).toBeTruthy();
  expect(staged.getByText("2 changes to save")).toBeTruthy();
  expect(staged.getByText("into production")).toBeTruthy();
  cleanup();
  open(canvasUrl);
  const deploying = await bar();
  expect(deploying.getByText("Deploying · Add worker")).toBeTruthy();
  fireEvent.click(deploying.getByRole("button", { name: "Save" }));
  const sheet = within(screen.getByRole("dialog", { name: "2 changes for production" }));
  expect(sheet.getByRole("region", { name: "web" }).textContent).toContain("web will be updated2 settings");
  expect(sheet.getByText("Nothing deploys yet. production gets 2 changes to deploy.")).toBeTruthy();
  // production changed it too since branching.
  expect(sheet.getAllByText("Changed in production too")).toHaveLength(1);
  expect(sheet.getByRole("switch", { name: "Delete fix-web after saving" })).toBeTruthy();
  expect(sheet.getByRole("button", { name: "Save to production" })).toBeTruthy();
  cleanup();
  startingPoint = { name: "recipe" };
  open(canvasUrl);
  const recipe = await bar();
  expect(recipe.getByText("recipe isn't deployed")).toBeTruthy();
  expect(recipe.getByRole("button", { name: "Save" })).toBeTruthy();
  cleanup();
  startingPoint = undefined;
  attempts = [];
  branch = branchReview([], 1);
  open(canvasUrl);
  const updates = await bar();
  expect(updates.getByText("1 update from production")).toBeTruthy();
  fireEvent.click(updates.getByRole("button", { name: "Update" }));
  expect(update).toHaveBeenCalledWith("env-1");
  cleanup();
  // Update waits for the Branch: Details opens the review page, which says why.
  unsettled = "Wait for this branch's deployment to finish.";
  open(canvasUrl);
  expect((await bar()).getByRole("link", { name: "Details" }).getAttribute("href")).toBe(`${canvasUrl}/review`);
  cleanup();
  branch = branchReview([], 0);
  open(canvasUrl);
  await act(async () => {});
  expect(screen.queryByRole("group", { name: "Bottom bar" })).toBeNull();
});

it("on a PR Environment, a row per Destination to save or saved; on a Destination, what goes live with a pull request", async () => {
  const saved = asTestDouble<ConditionalSaveRow>()({
    id: "save", prNumber: 142, prEnvironmentId: "env-1", destinationEnvironmentId: "env-0", rows: [{ row: imageRow("web:2") }, { row: imageRow("web:3") }],
  });
  const pullRequest = (save: ConditionalSaveRow | null) => asTestDouble<branchReviews.BranchReviewView>()({
    ...branchReview([], 0),
    pullRequest: { number: 142, repositoryId: 7, title: "Discounts", author: "maya", headBranch: "discounts", targetBranch: "main", commits: 2, closed: false, retired: false },
    goesTo: [{ destination: { id: "env-0", name: "production", namespace: "shop-production" }, rows: [imageRow("web:2"), imageRow("web:3")], review: "r", saved: save }],
    changes: save ? 0 : 2, environmentName: () => "production",
  });
  branch = pullRequest(null);
  open(canvasUrl);
  const unsaved = await bar();
  expect(unsaved.getByText("2 changes to save")).toBeTruthy();
  expect(unsaved.getByText("go live when PR #142 merges")).toBeTruthy();
  fireEvent.click(unsaved.getByRole("button", { name: "Save" }));
  const sheet = within(screen.getByRole("dialog", { name: "2 changes for production" }));
  expect(sheet.getByText("PR #142 · Discounts")).toBeTruthy();
  expect(sheet.getByText("web redeploys when PR #142 merges")).toBeTruthy();
  // No delete: shutting down is opt-in.
  expect(sheet.getAllByRole("switch").map((control) => control.getAttribute("aria-checked"))).toEqual(["false"]);
  expect(sheet.getByText("Shut down fix-web now")).toBeTruthy();
  expect(sheet.getByText("Starts again on the next push")).toBeTruthy();
  expect(sheet.getByRole("button", { name: "Save to production" })).toBeTruthy();
  cleanup();

  // Off, beside what it saved: Deploy starts it again.
  branch = pullRequest(saved);
  shutdown = "running";
  open(canvasUrl);
  expect((await bar()).getByText("Shutting down")).toBeTruthy();
  cleanup();
  shutdown = "failed";
  open(canvasUrl);
  fireEvent.click((await bar()).getByRole("button", { name: "Shut down" }));
  expect(shutDown).toHaveBeenCalledOnce();
  cleanup();
  shutdown = "off";
  open(canvasUrl);
  const offBar = await bar();
  expect(offBar.getByText("Off")).toBeTruthy();
  expect(offBar.getByText("Starts again on the next push")).toBeTruthy();
  expect(offBar.getByText("2 changes go live")).toBeTruthy();
  fireEvent.click(offBar.getByRole("button", { name: "Deploy" }));
  expect(start).toHaveBeenCalledOnce();
  cleanup();
  shutdown = null;

  open(canvasUrl);
  const waiting = await bar();
  expect(waiting.getByText("2 changes go live")).toBeTruthy();
  expect(waiting.getByText("when PR #142 merges")).toBeTruthy();
  // Undo is in Details.
  fireEvent.click(waiting.getByRole("button", { name: "Details" }));
  fireEvent.click(within(screen.getByRole("dialog", { name: "2 changes go live when PR #142 merges" })).getByRole("button", { name: "Undo" }));
  expect(withdraw).toHaveBeenCalledOnce();
  cleanup();

  held = [saved];
  // A Branch that's also a Destination: what goes live here, and beside it what it has to save.
  branch = branchReview([imageRow("web:2")], 0);
  open(canvasUrl);
  const both = await bar();
  expect(both.getByText("2 changes go live with PR #142")).toBeTruthy();
  expect(both.getByText("1 change to save")).toBeTruthy();
  cleanup();
  branch = null;
  open(canvasUrl);
  const quiet = await bar();
  expect(quiet.getByText("2 changes go live with PR #142")).toBeTruthy();
  expect(quiet.queryByRole("button", { name: /^Deploy/ })).toBeNull();
  fireEvent.click(quiet.getByRole("button", { name: "Details" }));
  const details = within(screen.getByRole("dialog", { name: "2 changes go live with PR #142" }));
  expect(details.getByText("web redeploys when PR #142 merges")).toBeTruthy();
  cleanup();

  // With changes to deploy, the saves still waiting are listed read-only in their Details.
  open(canvasUrl, [replicas], 1);
  const staged = await bar();
  expect(staged.queryByText("2 changes go live with PR #142")).toBeNull();
  fireEvent.click(staged.getByRole("button", { name: "Details" }));
  expect(within(screen.getByRole("dialog", { name: "Environment changes" })).getByRole("region", { name: "2 changes go live with PR #142" })).toBeTruthy();
});
