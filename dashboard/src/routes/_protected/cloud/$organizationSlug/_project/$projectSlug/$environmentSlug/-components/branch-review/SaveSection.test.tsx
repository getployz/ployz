// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as branchCommands from "#/modules/branches/branch-commands";
import * as documents from "#/modules/environment-design/environment-document.collection";
import * as workspaces from "#/modules/environment-design/workspace.queries";
import * as saveCommands from "#/modules/pr-environments/conditional-save-commands";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import type { ChangeRow } from "#/modules/branches/branch-review";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { asTestDouble } from "#/lib/test-double";
import { SaveSection } from "./SaveSection";

const withdraw = vi.fn();
const row = (path: string, from: string, conflict = false): ChangeRow => ({ key: `web-lineage:${path}`, role: "move", conflict, base: "1", from, into: "1" });
const rows = [row("source.image", "web:2", true), row("replicas", "2")];
const production = { id: "env-0", name: "production", namespace: "shop-production" };
const branch = (save: ChangeRow[]) => asTestDouble<BranchReviewView>()({
  parent: production, pullRequest: null, kept: false, save, saveReview: "r", changes: save.length, nameOf: () => "web",
});
const pullRequest = (saved: ConditionalSaveRow | null, closed = false) => asTestDouble<BranchReviewView>()({
  ...branch([]),
  pullRequest: { number: 142, repositoryId: 7, title: "Discounts", author: "maya", headBranch: "discounts", targetBranch: "main", commits: 2, closed, retired: false },
  goesTo: [{ destination: production, rows, review: "r", saved }],
  changes: saved ? 0 : 2,
});

beforeEach(() => {
  vi.spyOn(saveCommands, "useConditionalSave").mockImplementation(() =>
    asTestDouble<ReturnType<typeof saveCommands.useConditionalSave>>()({ save: { mutate: vi.fn(), isPending: false }, withdraw: { mutate: withdraw, isPending: false } }));
  vi.spyOn(branchCommands, "useSaveBranch").mockImplementation(() =>
    asTestDouble<ReturnType<typeof branchCommands.useSaveBranch>>()({ mutate: vi.fn(), isPending: false, isError: false }));
  vi.spyOn(documents, "useEnvironmentDocument").mockImplementation(() =>
    asTestDouble<ReturnType<typeof documents.useEnvironmentDocument>>()({ name: "fix-web", intent: { services: [] } }));
  vi.spyOn(workspaces, "useWorkspace").mockImplementation(() => asTestDouble<ReturnType<typeof workspaces.useWorkspace>>()({ projects: [] }));
});
afterEach(cleanup);

function open(review: BranchReviewView) {
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    component: () => <><h2>Panel</h2><SaveSection review={review} environmentId="env-1" /></> });
  render(<RouterProvider router={createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/fix-web"] }),
  })} />);
}

it("lists what Save would put in the Parent, and Save to it opens the sheet", async () => {
  open(branch(rows));
  const section = within(await screen.findByRole("region", { name: "For production" }));
  // production changed the image too since branching.
  expect(section.getAllByText("Changed in production too")).toHaveLength(1);
  fireEvent.click(section.getByRole("button", { name: "Save to production" }));
  const sheet = within(screen.getByRole("dialog", { name: "2 changes for production" }));
  expect(sheet.getByRole("region", { name: "web" }).textContent).toContain("web will be updated2 settings");
  expect(sheet.getByText("Nothing deploys yet. production gets 2 changes to deploy.")).toBeTruthy();
  expect(sheet.getByRole("switch", { name: "Delete fix-web after saving" })).toBeTruthy();
  expect(sheet.getByRole("button", { name: "Save to production" })).toBeTruthy();
  cleanup();

  open(branch([]));
  const empty = within(await screen.findByRole("region", { name: "For production" }));
  expect(empty.getByText("Nothing to save.")).toBeTruthy();
  expect(empty.queryByRole("button")).toBeNull();
});

it("on a PR Environment, each Destination: its changes with Save, or what it saved with Undo; nothing once the PR closes", async () => {
  open(pullRequest(null));
  const unsaved = within(await screen.findByRole("region", { name: "For production" }));
  expect(unsaved.getByText("Goes live when PR #142 merges.")).toBeTruthy();
  fireEvent.click(unsaved.getByRole("button", { name: "Save to production" }));
  const sheet = within(screen.getByRole("dialog", { name: "2 changes for production" }));
  expect(sheet.getByText("PR #142 · Discounts")).toBeTruthy();
  expect(sheet.getByText("web redeploys when PR #142 merges")).toBeTruthy();
  // No delete: shutting down is opt-in.
  expect(sheet.getAllByRole("switch").map((control) => control.getAttribute("aria-checked"))).toEqual(["false"]);
  expect(sheet.getByText("Shut down fix-web now")).toBeTruthy();
  cleanup();

  const saved = asTestDouble<ConditionalSaveRow>()({
    id: "save", prNumber: 142, prEnvironmentId: "env-1", destinationEnvironmentId: "env-0", rows: rows.map((candidate) => ({ row: candidate })),
  });
  open(pullRequest(saved));
  const waiting = within(await screen.findByRole("region", { name: "Saved for production" }));
  expect(waiting.getByText("Goes live when PR #142 merges.")).toBeTruthy();
  fireEvent.click(waiting.getByRole("button", { name: "Undo" }));
  expect(withdraw).toHaveBeenCalledOnce();
  cleanup();

  open(pullRequest(saved, true));
  await screen.findByText("Panel");
  expect(screen.queryByRole("region")).toBeNull();
});
