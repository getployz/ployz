// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ConfigCommand, ConfigWritten, EnvironmentView } from "@ployz/sdk";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "./store.functions";
import type { StoreResult } from "./store.contract";
import { diffQuery, environmentSettingsQuery, refetchStoreViews, storeViewOptions, useStoreView, withPendingChanges } from "./store-view.queries";
import { useStoreWriter, type StoreEdit } from "./store-write";

afterEach(() => vi.restoreAllMocks());

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
  ({ ok: true, value: { written: "edited", environment: { id: "env", project: "shop", name: "production", revision }, staged: ["web.replicas"], immediate: [] } });
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
  expect(test.write.mock.calls[0]?.[0]).toEqual({ data: { organizationSlug: "acme", command: { command: "edit", environment: ref, expect: 2, changes: replicas(3) } } });

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

it("shows pending sets and unsets as their values, in order, knowing no edit rules", () => {
  const shown = withPendingChanges(view(1, 1), [
    { op: "set", path: "web.replicas", value: 4 },
    { op: "unset", path: "web.startCommand" },
    { op: "set", path: "web.replicas", value: 6 },
  ]);
  expect(shown.settings.map((row) => row.value)).toEqual([6, null]);
});

it("runs a Move after pending edits in every Environment", async () => {
  const test = setup(view(2, 1));
  const edit = deferred<StoreResult<ConfigWritten>>();
  test.write.mockImplementationOnce(() => edit.promise).mockResolvedValueOnce({ ok: true, value: { written: "moved" } as never });
  act(() => { void test.edit({ environment: { project: "shop", environment: "staging" }, changes: replicas(3) }).isPersisted.promise.catch(() => {}); });
  const moved = test.writer.commit({ command: "move", move: "save", from: ref, into: null, picks: null });
  await new Promise((resolve) => setTimeout(resolve, 10));
  expect(test.write).toHaveBeenCalledTimes(1);
  edit.resolve(edited(3));
  await moved.isPersisted.promise;
  expect(test.write.mock.calls[1]?.[0]).toMatchObject({ data: { command: { command: "move" } } });
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
  const diff = { environment: view(3, 1).environment, version: "3:1:1", saved: 1, published: false, hints: [], total_count: 1, changes: [] };
  const key = storeViewOptions("acme", test.scope, diffQuery(ref)).queryKey;
  test.scope.queryClient.setQueryData<unknown>(key, { ok: true, value: { ...diff, total_count: 0 } });
  test.write.mockResolvedValueOnce({ ok: true, value: { written: "published" } as never, views: { diff } } as never);
  // No refetch answers: only the write's own answer can bring the count.
  vi.mocked(functions.readStoreViewServerFn).mockImplementation(() => new Promise(() => {}));
  test.writer.commit({ command: "publish", environment: ref, version: null, accept_volume_loss: [] });
  await waitFor(() => expect(test.scope.queryClient.getQueryData<{ value: { total_count: number } }>(key)?.value.total_count).toBe(1));
});

it("refetches only the views a changed Store table family backs", () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const settings = storeViewOptions("acme", scope, environmentSettingsQuery(ref)).queryKey;
  const diff = storeViewOptions("acme", scope, { query: "diff", environment: ref }).queryKey;
  const deployments = storeViewOptions("acme", scope, { query: "deployments", environment: ref, limit: null, cursor: null }).queryKey;
  for (const key of [settings, diff, deployments]) queryClient.setQueryData<unknown>(key, { ok: true, value: {} });

  refetchStoreViews("acme", scope, "store_deployment");
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
