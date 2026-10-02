// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type { ConfigCommand, DiffView, NodeChange } from "@ployz/sdk";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { changeGroups } from "#/modules/config-store/store-deployments";
import * as functions from "#/modules/config-store/store.functions";
import { EnvironmentChangesReview } from "../canvas/EnvironmentChangesReview";
import { storeHintNotes } from "./store-hints";

afterEach(() => { cleanup(); vi.restoreAllMocks(); document.body.replaceChildren(); });

const fixApi = { project: "shop", environment: "fix-api" };
const setting = (path: string, before: string, after: string) => ({ path, kind: "update" as const, before, after, canRestore: true });
const api = (settings: NodeChange["settings"]): NodeChange =>
  ({ type: "service", id: "api", name: "api", lifecycle: "update", settings }) as NodeChange;
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
  const write = vi.spyOn(functions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "moved" } } as never);
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
      <EnvironmentChangesReview groups={groups} totalChanges={view.total_count} canDeploy canPublish={false} onPublish={() => {}}
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

const rowOf = async (label: string) => (await screen.findByText(label)).closest("tr") as HTMLElement;

it("tags what arrived with where it came from, and Never sync on it marks that setting", async () => {
  const test = open(diff({
    changes: [api([setting("api.env.CACHE_TTL", "60", "300"), setting("api.replicas", "1", "2")])], total_count: 2,
    incoming: [{ row: "api.env.CACHE_TTL", from: "production" }],
  }));

  const arrived = within(await rowOf("Environment variable CACHE_TTL"));
  expect(arrived.getByText("From production")).toBeTruthy();
  fireEvent.click(arrived.getByRole("button", { name: /^Never sync/u }));
  expect(test.neverSync).toHaveBeenCalledWith("api.env.CACHE_TTL");
  // fix-api's own change came from nowhere else.
  const own = within(await rowOf("Replicas"));
  expect(own.queryByText(/^From /u)).toBeNull();
  expect(own.queryByRole("button", { name: /^Never sync/u })).toBeNull();
});

it("matches a Volume's setting as the Store names it, without `volumes.`", async () => {
  const test = open(diff({
    changes: [{ type: "volume", id: "data", name: "data", lifecycle: "update", settings: [setting("volumes.data.name", "pg", "pg-2")] } as NodeChange],
    total_count: 1, incoming: [{ row: "data.name", from: "production" }],
  }));

  fireEvent.click(within(await rowOf("Name")).getByRole("button", { name: /^Never sync/u }));
  expect(test.neverSync).toHaveBeenCalledWith("volumes.data.name");
});

it("tags a whole node that arrived on the node, which can't be marked", async () => {
  const test = open(diff({
    changes: [{ ...api([setting("api.replicas", "1", "2")]), lifecycle: "create" }], total_count: 2,
    incoming: [{ row: "api", from: "staging" }],
  }));

  expect(await screen.findByText("From staging")).toBeTruthy();
  expect(screen.queryByRole("button", { name: /^Never sync/u })).toBeNull();
  expect(test.neverSync).not.toHaveBeenCalled();
});

it("offers the Parent's value beside the Branch's own change, and Use stages it", async () => {
  const test = open(diff({
    changes: [api([setting("api.env.LOG_LEVEL", "info", "debug")])], total_count: 1,
    follow_hints: [{ from: "production", row: "api.env.LOG_LEVEL", value: "warn" }],
  }));

  const own = within(await rowOf("Environment variable LOG_LEVEL"));
  expect(own.getByText(/production:/u).textContent).toContain("warn");
  fireEvent.click(own.getByRole("button", { name: "Use production's api.env.LOG_LEVEL" }));
  await waitFor(() => expect(test.commands()).toEqual([
    { command: "move", move: "take", from: "production", into: fixApi, rows: ["api.env.LOG_LEVEL"], version: "4:abc" },
  ]));
});

it("keeps a discarded change from the Parent after the changes, with Use", async () => {
  const test = open(diff({
    changes: [api([setting("api.replicas", "1", "2")])], total_count: 1,
    follow_hints: [{ from: "production", row: "api.env.CACHE_TTL", value: "300" }],
  }));

  const after = within(await screen.findByRole("group", { name: "Not staged from production" }));
  expect(after.getByText("api · CACHE_TTL")).toBeTruthy();
  fireEvent.click(after.getByRole("button", { name: "Use production's api.env.CACHE_TTL" }));
  await waitFor(() => expect(test.commands()).toEqual([
    { command: "move", move: "take", from: "production", into: fixApi, rows: ["api.env.CACHE_TTL"], version: "4:abc" },
  ]));
});
