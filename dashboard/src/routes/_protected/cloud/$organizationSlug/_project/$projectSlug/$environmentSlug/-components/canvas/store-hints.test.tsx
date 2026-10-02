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
  ({ type: "service", id: "api", row: id("a:node"), name: "api", lifecycle: "update", settings }) as NodeChange;
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
    changes: [{ type: "volume", id: "data", row: id("d:node"), name: "data", lifecycle: "update", settings: [setting("volumes.data.name", "pg", "pg-2", "d:name")] } as NodeChange],
    total_count: 1, incoming: [{ row: id("d:name"), node: "volumes.data", kind: "volume", name: "name", from: "production" }],
  }));

  fireEvent.click((await menuOf("data Name")).getByRole("menuitem", { name: "Never sync" }));
  expect(test.neverSync).toHaveBeenCalledWith("volumes.data.name", "d:name");
});

it("joins each part of a composite setting to the row it falls in", async () => {
  const test = open(diff({
    changes: [api([setting("api.healthcheck.path", "/", "/up", "a:healthcheck"), setting("api.healthcheck.timeoutSeconds", "5", "9", "a:healthcheck")])],
    total_count: 2, incoming: [{ row: id("a:healthcheck"), node: "api", kind: "service", name: "healthcheck", from: "production" }],
  }));

  const incoming = within(await screen.findByRole("region", { name: "From production's deploy" }));
  expect(incoming.getAllByRole("listitem")).toHaveLength(2);
  fireEvent.click((await menuOf("api healthcheck.timeoutSeconds")).getByRole("menuitem", { name: "Never sync" }));
  expect(test.neverSync).toHaveBeenCalledWith("api.healthcheck.timeoutSeconds", "a:healthcheck");
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
