// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type { ConfigCommand, DiffView, NodeChange, RowId } from "@ployz/sdk";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { changeGroups } from "#/modules/config-store/store-deployments";
import * as functions from "#/modules/config-store/store.functions";
import { EnvironmentChangesReview } from "../canvas/EnvironmentChangesReview";
import { storeHintNotes } from "./store-hints";

afterEach(() => { cleanup(); vi.restoreAllMocks(); document.body.replaceChildren(); });

const fixApi = { project: "shop", environment: "fix-api" };
const id = (value: string) => value as RowId;
const setting = (path: string, before: string, after: string, row: string | null = null) =>
  ({ path, kind: "update" as const, before, after, canRestore: true, row: row === null ? null : id(row) });
const api = (settings: NodeChange["settings"]): NodeChange =>
  ({ type: "service", id: "api", row: id("a:node"), name: "api", lifecycle: "update", comparison: null, data: null, restarts: [], settings });
const diff = (extra: Partial<DiffView>): DiffView => ({
  environment: { id: "id-fix-api", project: "shop", name: "fix-api", revision: 4 }, version: "4:abc", saved: null,
  published: false, changes: [], total_count: 0, hints: [], incoming: [], follow_hints: [], ...extra,
});

/** fix-api's Details over `view`, as the bottom bar opens it. */
function open(view: DiffView) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  // SAFETY: after a write the writer refetches views these tests never read.
  vi.spyOn(functions, "readStoreViewServerFn").mockResolvedValue({ ok: true, value: {} } as never);
  const write = vi.spyOn(functions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "taken" } } as never);
  const neverSync = vi.fn();
  const groups = changeGroups(view, []);
  const notes = storeHintNotes(view, groups, neverSync);

  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({
    getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    loader: ({ params }) => ({ store: { project: params.projectSlug, environment: params.environmentSlug } }),
    component: () => (
      <EnvironmentChangesReview environment="fix-api" groups={groups} totalChanges={view.total_count} canDeploy canPublish={false} onPublish={() => {}}
        onDiscardAll={() => {}} onClose={() => {}} onDeploy={() => {}} message="" onMessageChange={() => {}}
        onDiscardNode={() => {}} onDiscardRow={() => {}} {...notes} />
    ),
  });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/fix-api"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  const commands = () => write.mock.calls.map(([call]): ConfigCommand | undefined => call?.data.command);
  return { neverSync, commands };
}

const rowOf = async (label: string) => (await screen.findByText(label)).closest("li") as HTMLElement;
/** The items of a change's ⋯ menu. */
async function menuOf(label: string) {
  fireEvent.click(await screen.findByRole("button", { name: `Actions for ${label}` }));
  return within(await screen.findByRole("menu"));
}

it("groups what the Parent's deploy brought apart from the Branch's own, and Never sync on it marks that setting", async () => {
  const test = open(diff({
    changes: [api([setting("api.env.CACHE_TTL", "60", "300", "a:variables.CACHE_TTL"), setting("api.replicas", "1", "2")])], total_count: 2,
    incoming: [{ row: id("a:variables.CACHE_TTL"), node: "api", kind: "service", name: "env.CACHE_TTL", from: "production" }],
  }));

  const incoming = within(await screen.findByRole("region", { name: "From production's deploy" }));
  expect(incoming.getByText("fix-api is a Branch of production, so what production deploys arrives here too.")).toBeTruthy();
  expect(incoming.getByText("CACHE_TTL")).toBeTruthy();
  expect(within(screen.getByRole("region", { name: "Your changes" })).getByText("Replicas")).toBeTruthy();
  // fix-api's own change came from nowhere else.
  const own = await menuOf("api Replicas");
  expect(own.queryByRole("menuitem", { name: "Never sync" })).toBeNull();
  fireEvent.keyDown(screen.getByRole("menu"), { key: "Escape" });
  fireEvent.click((await menuOf("api Environment variable CACHE_TTL")).getByRole("menuitem", { name: "Never sync" }));
  expect(test.neverSync).toHaveBeenCalledWith("api.env.CACHE_TTL", "a:variables.CACHE_TTL");
});

it("matches a Volume's setting by the row it falls in", async () => {
  const test = open(diff({
    changes: [{ type: "volume", id: "data", row: id("d:node"), name: "data", lifecycle: "update", comparison: null, data: null, restarts: [], settings: [setting("volumes.data.name", "pg", "pg-2", "d:name")] }],
    total_count: 1, incoming: [{ row: id("d:name"), node: "volumes.data", kind: "volume", name: "name", from: "production" }],
  }));

  fireEvent.click((await menuOf("data Name")).getByRole("menuitem", { name: "Never sync" }));
  expect(test.neverSync).toHaveBeenCalledWith("volumes.data.name", "d:name");
});

it("groups a whole node that arrived, with its settings, under where it came from; it can't be marked", async () => {
  const test = open(diff({
    changes: [{ ...api([setting("api.replicas", "1", "2")]), lifecycle: "create" }], total_count: 2,
    incoming: [{ row: id("a:node"), node: "api", kind: "service", name: null, from: "production" }],
  }));

  const incoming = within(await screen.findByRole("region", { name: "From production's deploy" }));
  expect(incoming.getByText("api · will be added")).toBeTruthy();
  expect(incoming.getByText("Replicas")).toBeTruthy();
  expect((await menuOf("api")).queryByRole("menuitem", { name: "Never sync" })).toBeNull();
  expect(test.neverSync).not.toHaveBeenCalled();
});

it("offers the Parent's value beside the Branch's own change, and Use stages it", async () => {
  const test = open(diff({
    changes: [api([setting("api.env.LOG_LEVEL", "info", "debug", "a:variables.LOG_LEVEL")])], total_count: 1,
    follow_hints: [{ from: "production", row: id("a:variables.LOG_LEVEL"), node: "api", kind: "service", name: "env.LOG_LEVEL", value: "warn" }],
  }));

  const own = within(await rowOf("LOG_LEVEL"));
  expect(own.getByText(/production has since set/u).textContent).toContain("warn");
  fireEvent.click(own.getByRole("button", { name: "Use theirs: production's api LOG_LEVEL" }));
  await waitFor(() => expect(test.commands()).toEqual([
    { command: "take", from: "production", into: fixApi, rows: ["a:variables.LOG_LEVEL"], version: "4:abc" },
  ]));
});

it("keeps hints no change shows after the changes, both kinds in one list, each Use taking the version the user saw", async () => {
  const test = open(diff({
    changes: [api([setting("api.replicas", "1", "2")])], total_count: 1,
    follow_hints: [{ from: "production", row: id("a:variables.CACHE_TTL"), node: "api", kind: "service", name: "env.CACHE_TTL", value: "300" }],
    hints: [{ conditional_sync: "cs1", pull_request: 142, row: id("d:name"), node: "volumes.data", kind: "volume", name: "name", value: "pg-2", landed: "hint" }],
  }));

  const after = within(await screen.findByRole("group", { name: "Not among these changes" }));
  expect(after.getByText("api · CACHE_TTL")).toBeTruthy();
  expect(after.getByText("data · Name")).toBeTruthy();
  fireEvent.click(after.getByRole("button", { name: "Use theirs: production's api CACHE_TTL" }));
  fireEvent.click(after.getByRole("button", { name: "Use PR #142's data Name" }));
  await waitFor(() => expect(test.commands()).toEqual([
    { command: "take", from: "production", into: fixApi, rows: ["a:variables.CACHE_TTL"], version: "4:abc" },
    { command: "take", from: "cs1", into: fixApi, rows: ["d:name"], version: "4:abc" },
  ]));
});

it("renders Config Follow and PR hints as files while Use retains the exact Store rows", async () => {
  const privateFile = { content: "CONFIG_HINT_PRIVATE_SENTINEL", mode: "0440", uid: 1234, gid: 5678 };
  const row = { row: id("c:files.nested/app.conf.bak"), node: "configs.app-settings", kind: "config" as const, name: "files.nested/app.conf.bak", value: privateFile };
  const test = open(diff({
    follow_hints: [{ ...row, from: "production" }],
    hints: [{ ...row, row: id("c:files.app.conf"), name: "files.app.conf", conditional_sync: "cs-config", pull_request: 142, landed: "hint" }],
  }));

  const after = within(await screen.findByRole("group", { name: "Not among these changes" }));
  expect(after.getByText("app-settings · nested/app.conf.bak")).toBeTruthy();
  expect(after.getByText("app-settings · app.conf")).toBeTruthy();
  for (const summary of after.getAllByText("File")) expect(summary.closest(".ph-no-capture")).toBeTruthy();
  expect(after.queryByText(/CONFIG_HINT_PRIVATE_SENTINEL|0440|1234|5678/u)).toBeNull();
  fireEvent.click(after.getByRole("button", { name: "Use theirs: production's app-settings nested/app.conf.bak" }));
  fireEvent.click(after.getByRole("button", { name: "Use PR #142's app-settings app.conf" }));
  await waitFor(() => expect(test.commands()).toEqual([
    { command: "take", from: "production", into: fixApi, rows: ["c:files.nested/app.conf.bak"], version: "4:abc" },
    { command: "take", from: "cs-config", into: fixApi, rows: ["c:files.app.conf"], version: "4:abc" },
  ]));
});

it("keeps a staged Config PR hint summary outside capture", async () => {
  open(diff({ hints: [{ conditional_sync: "cs-config", pull_request: 142, row: id("c:files.app.conf"), node: "configs.app-settings", kind: "config", name: "files.app.conf", value: { content: "STAGED_CONFIG_PRIVATE_SENTINEL", mode: "0444", uid: 0, gid: 0 }, landed: "staged" }] }));

  const after = within(await screen.findByRole("group", { name: "Not among these changes" }));
  expect(after.getByText("app-settings · app.conf")).toBeTruthy();
  expect(after.getByText("File").closest(".ph-no-capture")).toBeTruthy();
  expect(after.getByText("From PR #142")).toBeTruthy();
  expect(after.queryByText(/STAGED_CONFIG_PRIVATE_SENTINEL/u)).toBeNull();
});

it("summarizes Follow and PR hints attached to a changed Config file", async () => {
  const file = { mode: "0444", uid: 0, gid: 0 };
  const row = { row: id("c:files.app.conf"), node: "configs.app-settings", kind: "config" as const, name: "files.app.conf" };
  const test = open(diff({
    changes: [{ type: "config", id: "c", row: id("c:node"), name: "app-settings", lifecycle: "update", comparison: null, data: null, restarts: [], settings: [
      { path: "configs.app-settings.files.app.conf", kind: "update", before: { ...file, content: "original" }, after: { ...file, content: "own edit" }, canRestore: true, row: row.row },
    ] }], total_count: 1,
    follow_hints: [{ ...row, from: "production", value: { ...file, content: "ATTACHED_FOLLOW_PRIVATE_SENTINEL" } }],
    hints: [{ ...row, conditional_sync: "cs-config", pull_request: 142, value: { ...file, content: "ATTACHED_PR_PRIVATE_SENTINEL" }, landed: "hint" }],
  }));

  const own = within(await rowOf("app.conf"));
  expect(own.getByText("own edit")).toBeTruthy();
  for (const summary of own.getAllByText("File")) expect(summary.closest(".ph-no-capture")).toBeTruthy();
  expect(own.queryByText(/ATTACHED_(FOLLOW|PR)_PRIVATE_SENTINEL/u)).toBeNull();
  fireEvent.click(own.getByRole("button", { name: "Use theirs: production's app-settings app.conf" }));
  fireEvent.click(own.getByRole("button", { name: "Use PR #142's app-settings app.conf" }));
  await waitFor(() => expect(test.commands()).toEqual([
    { command: "take", from: "production", into: fixApi, rows: ["c:files.app.conf"], version: "4:abc" },
    { command: "take", from: "cs-config", into: fixApi, rows: ["c:files.app.conf"], version: "4:abc" },
  ]));
});
