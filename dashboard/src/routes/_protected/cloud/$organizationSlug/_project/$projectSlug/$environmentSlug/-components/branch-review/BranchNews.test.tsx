// @vitest-environment jsdom

import type { ReactNode } from "react";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as branchCollections from "#/modules/branches/branch.collection";
import * as branchCommands from "#/modules/branches/branch-commands";
import * as documents from "#/modules/environment-design/environment-document.collection";
import * as workspaces from "#/modules/environment-design/workspace.queries";
import * as saveCommands from "#/modules/pr-environments/conditional-save-commands";
import * as offCommands from "#/modules/pr-environments/off-commands";
import { branchNews } from "#/modules/branches/branch-news";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import type { ChangeRow } from "#/modules/branches/branch-review";
import type { ConditionalSaveRow, PrShutdown } from "#/modules/pr-environments/tables";
import { asTestDouble } from "#/lib/test-double";
import { BranchNewsList, ShutdownRow } from "./BranchNews";

const withdraw = vi.fn();
const update = vi.fn();
const start = vi.fn();
const shutDown = vi.fn();
let unsettled: string | null = null;
const row = (path: string, from: string, conflict = false): ChangeRow => ({ key: `web-lineage:${path}`, role: "move", conflict, base: "1", from, into: "1" });
const rows = [row("source.image", "web:2", true), row("replicas", "2")];
const production = { id: "env-0", name: "production", namespace: "shop-production" };
const branch = (save: ChangeRow[], updates: ChangeRow[] = []) => asTestDouble<BranchReviewView>()({
  parent: production, pullRequest: null, kept: false, save, saveReview: "r", changes: save.length, check: null,
  update: updates, updates: updates.length, live: [], differ: [], goesTo: [], nameOf: () => "web", environmentName: () => "fix-web",
});
const pullRequest = (saved: ConditionalSaveRow | null) => asTestDouble<BranchReviewView>()({
  ...branch([]),
  pullRequest: { number: 142, repositoryId: 7, title: "Discounts", author: "maya", headBranch: "discounts", targetBranch: "main", commits: 2, closed: false, retired: false },
  goesTo: [{ destination: production, rows, review: "r", saved }],
  changes: saved ? 0 : 2,
});

beforeEach(() => {
  unsettled = null;
  vi.spyOn(saveCommands, "useConditionalSave").mockImplementation(() =>
    asTestDouble<ReturnType<typeof saveCommands.useConditionalSave>>()({ save: { mutate: vi.fn(), isPending: false }, withdraw: { mutate: withdraw, isPending: false } }));
  vi.spyOn(branchCommands, "useSaveBranch").mockImplementation(() =>
    asTestDouble<ReturnType<typeof branchCommands.useSaveBranch>>()({ mutate: vi.fn(), isPending: false, isError: false }));
  vi.spyOn(branchCommands, "useUpdateBranch").mockImplementation(() => ({ update, makeOwnCopy: vi.fn() }));
  vi.spyOn(branchCollections, "useBranchUnsettled").mockImplementation(() => unsettled);
  vi.spyOn(branchCollections, "useKeepBranch").mockImplementation(() => vi.fn());
  vi.spyOn(offCommands, "usePrEnvironmentOff").mockImplementation(() => asTestDouble<ReturnType<typeof offCommands.usePrEnvironmentOff>>()({
    start: { mutate: start, isPending: false }, shutDown: { mutate: shutDown, isPending: false },
  }));
  vi.spyOn(documents, "useEnvironmentDocument").mockImplementation(() =>
    asTestDouble<ReturnType<typeof documents.useEnvironmentDocument>>()({ name: "fix-web", intent: { services: [] } }));
  vi.spyOn(workspaces, "useWorkspace").mockImplementation(() => asTestDouble<ReturnType<typeof workspaces.useWorkspace>>()({ projects: [] }));
});
afterEach(cleanup);

function open(content: ReactNode) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", component: () => content });
  render(<RouterProvider router={createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/fix-web"] }),
  })} />);
}
const news = (review: BranchReviewView, shutdown: PrShutdown | null = null) =>
  open(<BranchNewsList news={branchNews(review, shutdown, null)} review={review} environmentId="env-1" name="fix-web" />);

it("leads with the one thing to do next, the only solid button; the rest are a line each, their changes a tap away", async () => {
  news(branch(rows, [row("source.image", "web:3", true)]));
  const save = await screen.findByRole("button", { name: "Save to production" });
  const updating = screen.getByRole("button", { name: "Update" });
  expect(save.getAttribute("data-variant")).toBe("default");
  expect(updating.getAttribute("data-variant")).toBe("outline");
  // Values stay out of the lines: the updates just name where they are, and warn about what they'd replace.
  expect(screen.getByText("2 changes to save")).toBeTruthy();
  expect(screen.getByText("· 1 changed in fix-web too")).toBeTruthy();
  expect(screen.queryByText("Changed in production too")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /^2 changes to save/ }));
  expect(screen.getAllByText("Changed in production too")).toHaveLength(1);

  fireEvent.click(save);
  const sheet = within(screen.getByRole("dialog", { name: "2 changes for production" }));
  expect(sheet.getByRole("region", { name: "web" }).textContent).toContain("web will be updated2 settings");
  expect(sheet.getByText("Nothing deploys yet. production gets 2 changes to deploy.")).toBeTruthy();
  expect(sheet.getByRole("switch", { name: "Delete fix-web after saving" })).toBeTruthy();
  fireEvent.click(sheet.getByRole("button", { name: "Close" }));
  fireEvent.click(updating);
  expect(update).toHaveBeenCalledWith("env-1");
  cleanup();

  // While Update must wait, its line says why instead of offering it.
  unsettled = "Deploy this branch first.";
  news(branch([], [row("source.image", "web:3")]));
  expect(await screen.findByText("Deploy this branch first.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Update" })).toBeNull();
  cleanup();

  news(branch([]));
  expect(await screen.findByText("Up to date with production")).toBeTruthy();
  expect(screen.queryByRole("button")).toBeNull();
});

it("on a PR Environment, saves each Destination for the merge, and Undo takes a save back", async () => {
  news(pullRequest(null));
  expect(await screen.findByText("web · go live when #142 merges")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Save to production" }));
  const sheet = within(screen.getByRole("dialog", { name: "2 changes for production" }));
  expect(sheet.getByText("PR #142 · Discounts")).toBeTruthy();
  expect(sheet.getByText("web redeploys when PR #142 merges")).toBeTruthy();
  // No delete: shutting down is opt-in.
  expect(sheet.getAllByRole("switch").map((control) => control.getAttribute("aria-checked"))).toEqual(["false"]);
  cleanup();

  news(pullRequest(asTestDouble<ConditionalSaveRow>()({
    id: "save", prNumber: 142, prEnvironmentId: "env-1", destinationEnvironmentId: "env-0", rows: rows.map((candidate) => ({ row: candidate })),
  })));
  expect(await screen.findByText("Saved for production")).toBeTruthy();
  expect(screen.getByText("2 changes · go live when #142 merges")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(withdraw).toHaveBeenCalledOnce();
});

it("says a PR Environment is shutting down, runs a failed shutdown again, and deploys it once it's Off", async () => {
  const shutdown = (state: PrShutdown) => open(<ShutdownRow environmentId="env-1" name="pr-142" shutdown={state} />);
  shutdown("running");
  expect(await screen.findByText("Deploy once it's off")).toBeTruthy();
  expect(screen.queryByRole("button")).toBeNull();
  cleanup();
  shutdown("failed");
  expect(await screen.findByText("Some services may still run")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Shut down again" }));
  expect(shutDown).toHaveBeenCalledOnce();
  cleanup();
  shutdown("off");
  fireEvent.click(await screen.findByRole("button", { name: "Deploy pr-142" }));
  expect(start).toHaveBeenCalledOnce();
});
