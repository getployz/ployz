// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { ConfigCommand, ConfigWritten, DiffView, EnvironmentView } from "@ployz/sdk";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "./store.functions";
import type { StoreResult } from "./store.contract";
import { diffQuery, environmentSettingsQuery, environmentsQuery, refetchStoreViews, storeViewOptions, useStoreView } from "./store-view.queries";
import { useStoreRenamePending, withPendingChanges } from "./store-pending";
import { useStoreWriter, type StoreEdit } from "./store-write";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const ref = { project: "shop", environment: "production" };

function view(revision: number, replicas: number, values?: EnvironmentView["values"]): EnvironmentView {
  return {
    environment: { id: "env", project: "shop", name: "production", revision },
    settings: [
      { path: "web.replicas", value: replicas, default: 1, apply: "staged" },
      { path: "web.startCommand", value: "serve", default: null, apply: "staged" },
    ],
    values,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((yes) => { resolve = yes; });
  return { promise, resolve };
}

/** A Store holding `web` in shop/production, and a tab showing its Settings. */
function setup(initial: EnvironmentView) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const store = { current: initial };
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(async () =>
    // SAFETY: the mock answers the one query these tests read.
    ({ ok: true, value: { view: "environment", ...store.current } }) as never);
  const write = vi.spyOn(functions, "writeStoreServerFn");
  const query = environmentSettingsQuery(ref);
  queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, query).queryKey, { ok: true, value: { view: "environment", ...initial } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const shown = renderHook(() => useStoreView("acme", query), { wrapper });
  const replicas = () => {
    const result = shown.result.current;
    return result.ok ? result.value.settings.find((row) => row.path === "web.replicas")?.value : undefined;
  };
  const value = (path: string) => {
    const result = shown.result.current;
    return result.ok ? result.value.settings.find((row) => row.path === path)?.value : undefined;
  };
  const writer = renderHook(() => useStoreWriter("acme"), { wrapper }).result.current;
  return { scope, store, write, replicas, value, writer, edit: (edit: StoreEdit) => writer.edit(edit) };
}

const edited = (revision: number): StoreResult<ConfigWritten> =>
  ({ ok: true, value: { written: "edited", environment: { id: "env", project: "shop", name: "production", revision }, staged: ["web.replicas"], immediate: [], typed_addresses: [] } });
const replicas = (value: number) => [{ op: "set" as const, path: "web.replicas", value }];

it("shows an edit at once, saves edits in order against the newest revision, and keeps them once committed", async () => {
  const test = setup(view(2, 1));
  const first = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => first.promise).mockImplementationOnce(async () => {
    test.store.current = view(4, 5);
    return edited(4);
  });

  let one!: ReturnType<ReturnType<typeof useStoreWriter>["edit"]>;
  let two!: ReturnType<ReturnType<typeof useStoreWriter>["edit"]>;
  act(() => { one = test.edit({ environment: ref, changes: replicas(3) }); });
  await waitFor(() => expect(test.replicas()).toBe(3));
  act(() => { two = test.edit({ environment: ref, changes: replicas(5) }); });
  await waitFor(() => expect(test.replicas()).toBe(5));
  // The second waits for the first: one queue per Environment.
  expect(test.write).toHaveBeenCalledTimes(1);
  expect(test.write.mock.calls[0]?.[0]).toEqual({ data: { organizationSlug: "acme", command: { command: "edit", environment: ref, expect: 2, changes: replicas(3) }, shows: [environmentSettingsQuery(ref)] } });

  test.store.current = view(3, 3);
  first.resolve(edited(3));
  await one.isPersisted.promise;
  await two.isPersisted.promise;
  // It expects the revision its own first save produced.
  expect(test.write.mock.calls[1]?.[0]).toMatchObject({ data: { command: { expect: 3 } } });
  expect(test.replicas()).toBe(5);
});

it("undoes an edit the Store refuses as a conflict and shows what changed elsewhere", async () => {
  const test = setup(view(2, 1));
  const error = vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const refused = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => refused.promise);

  let edit!: ReturnType<ReturnType<typeof useStoreWriter>["edit"]>;
  act(() => { edit = test.edit({ environment: ref, changes: replicas(3) }); });
  await waitFor(() => expect(test.replicas()).toBe(3));
  // The CLI moved Working State before this tab heard about it.
  test.store.current = view(3, 7);
  refused.resolve({ ok: false, refusal: { code: "conflict", message: "Working State moved", details: { revision: 3 } } });
  await expect(edit.isPersisted.promise).rejects.toMatchObject({ code: "conflict", details: { revision: 3 } });
  await waitFor(() => expect(test.replicas()).toBe(7));
  expect(error).toHaveBeenCalledWith("Changed elsewhere, so this edit was undone. You're seeing the latest now.");
});

it("words a refused rename as the Store does: a referenced variable isn't a stale revision", async () => {
  const test = setup(view(2, 1));
  const error = vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const message = "web references web.API_URL (UPSTREAM); remove those references first";
  test.write.mockResolvedValueOnce({ ok: false, refusal: { code: "conflict", message, details: { service: "web", references: "web.API_URL", variables: ["UPSTREAM"] } } });
  let edit!: ReturnType<ReturnType<typeof useStoreWriter>["edit"]>;
  act(() => { edit = test.edit({ environment: ref, changes: [{ op: "rename", path: "web.env.API_URL", to: "PUBLIC_API_URL" }] }); });
  await expect(edit.isPersisted.promise).rejects.toMatchObject({ code: "conflict" });
  expect(error).toHaveBeenCalledWith(message);
});

it("says a signed-out edit wasn't saved and offers to sign in", async () => {
  const test = setup(view(2, 1));
  const error = vi.spyOn(toast, "error").mockImplementation(() => "toast");
  test.write.mockImplementationOnce(() => Promise.reject({ _tag: "PublicError", code: "UNAUTHORIZED", message: "Authentication is required." }));
  let edit!: ReturnType<ReturnType<typeof useStoreWriter>["edit"]>;
  act(() => { edit = test.edit({ environment: ref, changes: replicas(3) }); });
  await expect(edit.isPersisted.promise).rejects.toMatchObject({ code: "UNAUTHORIZED" });
  expect(error).toHaveBeenCalledWith("You're signed out. Nothing was saved.", expect.objectContaining({ action: expect.objectContaining({ label: "Sign in" }) }));
  await waitFor(() => expect(test.replicas()).toBe(1));
});

it("seals a variable at once, and a refused seal leaves it as it was", async () => {
  const plain = view(2, 1);
  plain.settings.push({ path: "web.env.TOKEN", value: "abc", default: null, apply: "staged" });
  const test = setup(plain);
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const refused = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => refused.promise);

  let edit!: ReturnType<ReturnType<typeof useStoreWriter>["edit"]>;
  const seal = [{ op: "set" as const, path: "web.env.TOKEN", value: { secret: "abc" } }];
  act(() => { edit = test.edit({ environment: ref, changes: seal }); });
  await waitFor(() => expect(test.value("web.env.TOKEN")).toEqual({ secret: true }));
  refused.resolve({ ok: false, refusal: { code: "invalid_argument", message: "Could not seal", details: null } });
  await expect(edit.isPersisted.promise).rejects.toMatchObject({ code: "invalid_argument" });
  await waitFor(() => expect(test.value("web.env.TOKEN")).toBe("abc"));
});

it("drops an echoed value the Store refuses, back to the value it holds", async () => {
  const test = setup(view(2, 1));
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const refused = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => refused.promise);

  let edit!: ReturnType<ReturnType<typeof useStoreWriter>["edit"]>;
  act(() => { edit = test.edit({ environment: ref, changes: replicas(9) }); });
  await waitFor(() => expect(test.replicas()).toBe(9));
  refused.resolve({ ok: false, refusal: { code: "invalid_argument", message: "replicas must be at most 8", details: null } });
  await expect(edit.isPersisted.promise).rejects.toMatchObject({ code: "invalid_argument" });
  await waitFor(() => expect(test.replicas()).toBe(1));
});

it("shows pending sets as their values, in order, and leaves unsets to the Store's answer", () => {
  const shown = withPendingChanges(view(1, 1), [
    { op: "set", path: "web.replicas", value: 4 },
    { op: "unset", path: "web.startCommand" },
    { op: "set", path: "web.replicas", value: 6 },
  ]);
  expect(shown.settings.map((row) => row.value)).toEqual([6, "serve"]);
});

it("runs a Sync after pending edits in every Environment", async () => {
  const test = setup(view(2, 1));
  const edit = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => edit.promise).mockResolvedValueOnce({ ok: true, value: { written: "synced" } as never });
  act(() => { void test.edit({ environment: { project: "shop", environment: "staging" }, changes: replicas(3) }).isPersisted.promise.catch(() => {}); });
  const synced = test.writer.commit({ command: "sync", from: ref, into: null, version: "1", picks: null });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(test.write).toHaveBeenCalledTimes(1);
  edit.resolve(edited(3));
  await synced.isPersisted.promise;
  expect(test.write.mock.calls[1]?.[0]).toMatchObject({ data: { command: { command: "sync" } } });
});

it("runs a Sync after a pending Batch that edits, as after an edit", async () => {
  const test = setup(view(2, 1));
  const batch = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => batch.promise).mockResolvedValueOnce({ ok: true, value: { written: "synced" } as never });
  const staging = { project: "shop", environment: "staging" };
  act(() => {
    void test.writer.commit({ command: "batch", environment: staging, commands: [{ command: "edit", environment: staging, expect: null, changes: replicas(3) }] })
      .isPersisted.promise.catch(() => {});
  });
  const synced = test.writer.commit({ command: "sync", from: ref, into: null, version: "1", picks: null });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(test.write).toHaveBeenCalledTimes(1);
  batch.resolve({ ok: true, value: { written: "batch", results: [] } as never });
  await synced.isPersisted.promise;
  expect(test.write.mock.calls[1]?.[0]).toMatchObject({ data: { command: { command: "sync" } } });
});

it("runs a copied node after the edits made before it, not the ones queued behind it", async () => {
  const test = setup(view(2, 1));
  const first = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => first.promise)
    .mockResolvedValueOnce({ ok: true, value: { written: "service", environment: view(4, 1).environment } as never })
    .mockResolvedValueOnce(edited(5));
  const one = test.edit({ environment: ref, changes: replicas(3) });
  const copied = test.writer.commit({ command: "copy_node", environment: ref, node: "web", expect: null });
  const two = test.edit({ environment: ref, changes: replicas(4) });
  first.resolve(edited(3));
  await Promise.all([one.isPersisted.promise, copied.isPersisted.promise, two.isPersisted.promise]);
  expect(test.write.mock.calls.map((call) => call[0]?.data.command)).toMatchObject([
    { command: "edit", expect: 2 }, { command: "copy_node", expect: 3 }, { command: "edit", expect: 4 },
  ]);
});

it("puts the write's committed review in the cache as it answers, before any refetch", async () => {
  const test = setup(view(2, 1));
  const diff = { environment: view(3, 1).environment, version: "3:1:1", saved: 1, holds_staged: false, incoming: [], follow_hints: [], total_count: 1, changes: [] };
  const key = storeViewOptions("acme", test.scope, diffQuery(ref)).queryKey;
  test.scope.queryClient.setQueryData<unknown>(key, { ok: true, value: { ...diff, total_count: 0 } });
  test.write.mockResolvedValueOnce({ ok: true, value: { written: "published" } as never, answers: [{ query: diffQuery(ref), result: { ok: true, value: { view: "diff", ...diff } } }] } as never);
  // No refetch answers: only the write's own answer can bring the count.
  vi.mocked(functions.readStoreViewServerFn).mockImplementation(() => new Promise(() => {}));
  test.writer.commit({ command: "publish", environment: ref, version: null, accept_volume_loss: [] });
  await waitFor(() => expect(test.scope.queryClient.getQueryData<{ value: { total_count: number } }>(key)?.value.total_count).toBe(1));
});

it("rereads a view the write's answer says the Store was too busy to read, keeping its rows meanwhile", async () => {
  const test = setup(view(2, 1));
  const read = vi.mocked(functions.readStoreViewServerFn);
  const reread = deferred<unknown>();
  read.mockImplementationOnce(() => reread.promise as never);
  const busy = { ok: false, refusal: { code: "unavailable", message: "The Store is busy", details: null } };
  test.write.mockResolvedValueOnce({ ...edited(3), answers: [{ query: environmentSettingsQuery(ref), result: busy }] } as never);
  const key = storeViewOptions("acme", test.scope, environmentSettingsQuery(ref)).queryKey;
  const persisted = test.edit({ environment: ref, changes: replicas(3) }).isPersisted.promise;
  await waitFor(() => expect(read).toHaveBeenCalledTimes(1));
  expect(test.scope.queryClient.getQueryData(key)).toEqual({ ok: true, value: { view: "environment", ...view(2, 1) } });
  reread.resolve({ ok: true, value: { view: "environment", ...view(3, 3) } });
  await persisted;
  expect(test.replicas()).toBe(3);
});

it("reports a rename pending in its Environment until the Store answers, and none elsewhere", async () => {
  const test = setup(view(2, 1));
  const answer = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => answer.promise);
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={test.scope.queryClient}>{children}</QueryClientProvider>;
  const here = renderHook(() => useStoreRenamePending("acme", ref), { wrapper });
  const elsewhere = renderHook(() => useStoreRenamePending("acme", { project: "shop", environment: "staging" }), { wrapper });
  const persisted = test.writer.commit({ command: "rename_service", environment: ref, service: "web", name: "site" }).isPersisted.promise;
  await waitFor(() => expect(here.result.current).toBe(true));
  expect(elsewhere.result.current).toBe(false);
  answer.resolve(edited(3));
  await persisted;
  await waitFor(() => expect(here.result.current).toBe(false));
});

it("sends no edit while a rename in its Environment saves, as it could name the old node, but lets a versioned discard through", async () => {
  const test = setup(view(2, 1));
  const error = vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const answer = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => answer.promise).mockResolvedValue(edited(4));
  const renamed = test.writer.commit({ command: "rename_service", environment: ref, service: "web", name: "site" }).isPersisted.promise;
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));

  await expect(test.writer.edit({ environment: ref, changes: [{ op: "unset", path: "web.mounts.data" }] }).isPersisted.promise)
    .rejects.toMatchObject({ code: "renaming" });
  expect(error).toHaveBeenCalledOnce();
  const discarded = test.writer.commit({ command: "discard", environment: ref, path: null, target: "saved", version: "4:2:1.0" }).isPersisted.promise;

  answer.resolve(edited(3));
  await Promise.all([renamed, discarded]);
  expect(test.write.mock.calls.map(([{ data }]) => data.command.command)).toEqual(["rename_service", "discard"]);
  await test.writer.edit({ environment: ref, changes: replicas(3) }).isPersisted.promise;
  expect(test.write).toHaveBeenCalledTimes(3);
});

it("locks only the renamed Project's Environments and writes while a Project rename saves", async () => {
  const test = setup(view(2, 1));
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const answer = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => answer.promise).mockResolvedValue(edited(4));
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={test.scope.queryClient}>{children}</QueryClientProvider>;
  const blog = { project: "blog", environment: "production" };
  const renamed = renderHook(() => useStoreRenamePending("acme", ref), { wrapper });
  const other = renderHook(() => useStoreRenamePending("acme", blog), { wrapper });
  const persisted = test.writer.commit({ command: "rename_project", project: "shop", name: "store" }).isPersisted.promise;
  await waitFor(() => expect(renamed.result.current).toBe(true));
  expect(other.result.current).toBe(false);

  await test.writer.edit({ environment: blog, changes: replicas(3) }).isPersisted.promise;
  await expect(test.writer.edit({ environment: ref, changes: replicas(3) }).isPersisted.promise).rejects.toMatchObject({ code: "renaming" });
  await expect(test.writer.commit({ command: "create_environment", id: "env_2", project: "shop", name: "staging" }).isPersisted.promise)
    .rejects.toMatchObject({ code: "renaming" });

  answer.resolve(edited(3));
  await persisted;
  await waitFor(() => expect(renamed.result.current).toBe(false));
  expect(test.write.mock.calls.map(([{ data }]) => data.command.command)).toEqual(["rename_project", "edit"]);
});

it("lists a new Branch in its Project's Environments once the write persists, so its canvas opens", async () => {
  const test = setup(view(2, 1));
  const listing = (names: string[]) => ({ ok: true as const, value: { view: "environments" as const, project: { name: "shop" }, environments: names.map((name) => ({ name })) } });
  const key = storeViewOptions("acme", test.scope, environmentsQuery("shop")).queryKey;
  test.scope.queryClient.setQueryData<unknown>(key, listing(["production"]));
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={test.scope.queryClient}>{children}</QueryClientProvider>;
  renderHook(() => useStoreView("acme", environmentsQuery("shop")), { wrapper });
  test.write.mockImplementationOnce(async ({ data }) => ({ ok: true, value: { written: "branch" } as never,
    answers: data.shows.filter((query) => query.query === "environments").map((query) => ({ query, result: listing(["production", "feature"]) as never })) }));
  await test.writer.commit({ command: "create_branch", id: "b", from: ref, name: "feature", copy: [], live: [], keep: false, setup: [], fix: null } as never).isPersisted.promise;
  expect(test.scope.queryClient.getQueryData(key)).toEqual(listing(["production", "feature"]));
});

it("rereads the Project's Environments after a new Default Environment, which names no Environment", async () => {
  const test = setup(view(2, 1));
  const listing = (fallback: string) => ({ ok: true as const, value: { view: "environments" as const, project: { name: "shop" },
    environments: ["production", "staging"].map((name) => ({ name, default: name === fallback })) } });
  const key = storeViewOptions("acme", test.scope, environmentsQuery("shop")).queryKey;
  test.scope.queryClient.setQueryData<unknown>(key, listing("production"));
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={test.scope.queryClient}>{children}</QueryClientProvider>;
  renderHook(() => useStoreView("acme", environmentsQuery("shop")), { wrapper });
  vi.mocked(functions.readStoreViewServerFn).mockImplementation(async ({ data }) =>
    // SAFETY: the Store now lists staging as the Default Environment; these tests read no other Project view.
    (data.query.query === "environments" ? listing("staging") : { ok: true, value: { view: "environment", ...test.store.current } }) as never);
  // The Store answers only views of the command's Environment; the Project's list names none.
  test.write.mockImplementationOnce(async () => ({ ok: true, value: { written: "project" } as never, answers: [] }));
  await test.writer.commit({ command: "set_default_environment", environment: { project: "shop", environment: "staging" } }).isPersisted.promise;
  expect(test.scope.queryClient.getQueryData(key)).toEqual(listing("staging"));
});

it("keeps a newer view when a write's answer or a read lands older", async () => {
  const test = setup(view(2, 1));
  const key = storeViewOptions("acme", test.scope, environmentSettingsQuery(ref)).queryKey;
  const answer = deferred<unknown>();
  test.write.mockImplementationOnce(() => answer.promise as never);
  const persisted = test.writer.commit({ command: "publish", environment: ref, version: null, accept_volume_loss: [] }).isPersisted.promise;
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));

  // A change-stream read lands revision 5 while the write's answer, read at 4, is on its way.
  test.scope.queryClient.setQueryData<unknown>(key, { ok: true, value: { view: "environment", ...view(5, 7) } });
  test.store.current = view(4, 3);
  answer.resolve({ ok: true, value: { written: "published" }, answers: [{ query: environmentSettingsQuery(ref), result: { ok: true, value: { view: "environment", ...view(4, 3) } } }] });
  await persisted;
  await act(async () => { await test.scope.queryClient.refetchQueries({ queryKey: key, exact: true }); });
  await waitFor(() => expect(test.replicas()).toBe(7));

  test.store.current = view(6, 8);
  await act(async () => { await test.scope.queryClient.refetchQueries({ queryKey: key, exact: true }); });
  await waitFor(() => expect(test.replicas()).toBe(8));

});

const diffAt = (revision: number, saved: number, head: string, total: number): StoreResult<{ view: "diff" } & DiffView> => ({ ok: true, value: { view: "diff",
  environment: view(revision, 1).environment, version: `${revision}:${saved}:${head}`, saved, holds_staged: false, incoming: [], follow_hints: [], included: [],
  total_count: total, changes: [] } });

it("orders a diff by its Saved revision, and lets a Deploy's ending replace it at the same revisions", async () => {
  const test = setup(view(6, 1));
  const options = storeViewOptions("acme", test.scope, diffQuery(ref));
  const key = options.queryKey;
  const total = () => test.scope.queryClient.getQueryData<{ value: { total_count: number } }>(key)?.value.total_count;
  const put = (result: StoreResult<DiffView>) => test.scope.queryClient.setQueryData<unknown>(key, result);
  await test.scope.queryClient.ensureQueryData({ ...options, initialData: diffAt(6, 2, "3.1", 2) });
  put(diffAt(6, 1, "3.1", 5));
  expect(total()).toBe(2);
  // The Deploy ended: Cloud's head token restarts, and every later read at the same revisions lands.
  put(diffAt(6, 2, "0.2", 0));
  expect(total()).toBe(0);
  put(diffAt(6, 2, "0.2", 1));
  expect(total()).toBe(1);
});

it("shows an Environment recreated under the name, not the deleted one's newer revision", () => {
  const test = setup(view(9, 4));
  const key = storeViewOptions("acme", test.scope, environmentSettingsQuery(ref)).queryKey;
  const recreated = view(1, 1);
  recreated.environment.id = "env-2";
  test.scope.queryClient.setQueryData<unknown>(key, { ok: true, value: { view: "environment", ...recreated } });
  expect(test.scope.queryClient.getQueryData<{ value: EnvironmentView }>(key)?.value.environment).toEqual(recreated.environment);
});

it("refetches only the views a changed Store table family backs", () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const settings = storeViewOptions("acme", scope, environmentSettingsQuery(ref)).queryKey;
  const diff = storeViewOptions("acme", scope, { query: "diff", environment: ref }).queryKey;
  const deployments = storeViewOptions("acme", scope, { query: "deployments", environment: ref, limit: null, cursor: null }).queryKey;
  for (const key of [settings, diff, deployments]) queryClient.setQueryData<unknown>(key, { ok: true, value: {} });

  refetchStoreViews("acme", scope, ["store_deployment"]);
  const invalidated = (key: readonly unknown[]) => queryClient.getQueryState(key)?.isInvalidated;
  expect([invalidated(settings), invalidated(diff), invalidated(deployments)]).toEqual([false, true, true]);
});

it("words a command's conflict as the Store does: a taken domain isn't a stale revision", async () => {
  const test = setup(view(2, 1));
  const error = vi.spyOn(toast, "error").mockImplementation(() => "toast");
  test.write.mockResolvedValueOnce({ ok: false, refusal: { code: "conflict", message: "Another Service already has this domain", details: {} } });
  const writer = renderHook(() => useStoreWriter("acme")).result.current;
  const added = writer.commit({ command: "add_domain", environment: ref, service: "web", hostname: "shop.acme.com", port: null });
  await expect(added.isPersisted.promise).rejects.toMatchObject({ code: "conflict" });
  expect(error).toHaveBeenCalledWith("Another Service already has this domain");
});

it("leaves a Deploy's confirmation_required to a caller that handles it, and toasts it for one that doesn't", async () => {
  const test = setup(view(2, 1));
  const error = vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const details = { volumes: [{ id: "v", name: "pg-data", docker_volume: "ns_vol-v", deletes: [{ machine_id: "m1", name: "ns_vol-v" }] }], accept: ["pg-data"], version: "9:1:0.1" };
  test.write.mockResolvedValueOnce({ ok: false, refusal: { code: "confirmation_required", message: "This Deploy permanently deletes the data of pg-data", details } });
  const writer = renderHook(() => useStoreWriter("acme")).result.current;
  const deploy = { command: "admit", admit: "deploy", id: "d", environment: ref, services: [], version: null, accept_volume_loss: [] } satisfies ConfigCommand;
  const admitted = writer.commit(deploy, ["confirmation_required"]);
  await expect(admitted.isPersisted.promise).rejects.toMatchObject({ code: "confirmation_required", details });
  expect(error).not.toHaveBeenCalled();
  test.write.mockResolvedValueOnce({ ok: false, refusal: { code: "confirmation_required", message: "This Deploy permanently deletes the data of pg-data", details } });
  await expect(writer.commit(deploy).isPersisted.promise).rejects.toMatchObject({ code: "confirmation_required" });
  expect(error).toHaveBeenCalledWith("This Deploy permanently deletes the data of pg-data");
});
