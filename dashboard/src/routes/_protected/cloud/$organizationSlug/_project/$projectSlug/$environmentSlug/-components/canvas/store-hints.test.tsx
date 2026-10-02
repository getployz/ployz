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
    changes: [api([setting("api.env.CACHE_TTL", "60", "300"), setting("api.replicas", "1", "2")])], total_count: 2,
    incoming: [{ row: "api.env.CACHE_TTL", path: "api.env.CACHE_TTL", whole: false, from: "production" }],
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
  expect(test.neverSync).toHaveBeenCalledWith("api.env.CACHE_TTL");
});

it("matches a Volume's setting by the path the receiver names it by", async () => {
  const test = open(diff({
    changes: [{ type: "volume", id: "data", name: "data", lifecycle: "update", settings: [setting("volumes.data.name", "pg", "pg-2")] } as NodeChange],
    total_count: 1, incoming: [{ row: "data.name", path: "volumes.data.name", whole: false, from: "production" }],
  }));

  fireEvent.click((await menuOf("data Name")).getByRole("menuitem", { name: "Never sync" }));
  expect(test.neverSync).toHaveBeenCalledWith("volumes.data.name");
});

it("groups a whole node that arrived, with its settings, under where it came from; it can't be marked", async () => {
  const test = open(diff({
    changes: [{ ...api([setting("api.replicas", "1", "2")]), lifecycle: "create" }], total_count: 2,
    incoming: [{ row: "api", path: "api", whole: true, from: "production" }],
  }));

  const incoming = within(await screen.findByRole("region", { name: "From production's deploy" }));
  expect(incoming.getByText("api · will be added")).toBeTruthy();
  expect(incoming.getByText("Replicas")).toBeTruthy();
  expect((await menuOf("api")).queryByRole("menuitem", { name: "Never sync" })).toBeNull();
  expect(test.neverSync).not.toHaveBeenCalled();
});

it("offers the Parent's value beside the Branch's own change, and Use stages it", async () => {
  const test = open(diff({
    changes: [api([setting("api.env.LOG_LEVEL", "info", "debug")])], total_count: 1,
    follow_hints: [{ from: "production", row: "api.env.LOG_LEVEL", path: "api.env.LOG_LEVEL", whole: false, value: "warn" }],
  }));

  const own = within(await rowOf("LOG_LEVEL"));
  expect(own.getByText(/production has since set/u).textContent).toContain("warn");
  fireEvent.click(own.getByRole("button", { name: "Use theirs: production's api.env.LOG_LEVEL" }));
  await waitFor(() => expect(test.commands()).toEqual([
    { command: "take", from: "production", into: fixApi, rows: ["api.env.LOG_LEVEL"], version: "4:abc" },
  ]));
});

it("keeps hints no change shows after the changes, both kinds in one list, each Use taking the version the user saw", async () => {
  const test = open(diff({
    changes: [api([setting("api.replicas", "1", "2")])], total_count: 1,
    follow_hints: [{ from: "production", row: "api.env.CACHE_TTL", path: "api.env.CACHE_TTL", whole: false, value: "300" }],
    hints: [{ conditional_sync: "cs1", pull_request: 142, row: "data.name", path: "volumes.data.name", whole: false, value: "pg-2", landed: "hint" }],
  }));

  const after = within(await screen.findByRole("group", { name: "Not among these changes" }));
  expect(after.getByText("api · CACHE_TTL")).toBeTruthy();
  expect(after.getByText("data · Name")).toBeTruthy();
  fireEvent.click(after.getByRole("button", { name: "Use theirs: production's api.env.CACHE_TTL" }));
  fireEvent.click(after.getByRole("button", { name: "Use PR #142's data.name" }));
  await waitFor(() => expect(test.commands()).toEqual([
    { command: "take", from: "production", into: fixApi, rows: ["api.env.CACHE_TTL"], version: "4:abc" },
    { command: "take", from: "cs1", into: fixApi, rows: ["volumes.data.name"], version: "4:abc" },
  ]));
});
