// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { orgStoreTables } from "#/collections/collections";
import { applyOrganizationChanges, watchOrganizationChanges } from "#/collections/org-changes.stream";
import { getDbClient } from "#/collections/scope";
import { organizationKeys } from "#/modules/organization/organization-state.queries";
import { environmentSettingsQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import { orgStoreSeed, orgStoreTableNames, subscribeOrgStoreTables } from "#/test/org-store-tables";

// The Org Store read answers with nothing new, so rereads settle without a network.
vi.mock("#/collections/read.functions", () => ({
  readCollectionServerFn: async () => ({ full: false, rows: [], deleted: [], cursor: "1" }),
}));

class FakeEventSource extends EventTarget {
  static latest: FakeEventSource | undefined;
  constructor() { super(); FakeEventSource.latest = this; }
  close() {}
}

const ORG_A = "00000000-0000-4000-8000-00000000000a";
const ORG_B = "00000000-0000-4000-8000-00000000000b";

afterEach(() => {
  vi.unstubAllGlobals();
});

it("reloads a tab holding a removed Organization's caches once the stream names the slug's new Organization", async () => {
  vi.stubGlobal("EventSource", FakeEventSource);
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  for (const get of Object.values(orgStoreTables)) vi.spyOn(get("acme", scope).utils, "refetch").mockResolvedValue([]);
  queryClient.setQueryData(["collections", "session", "user", "acme", "organization_support_access"], orgStoreSeed([{ id: ORG_A, allowed: true }]));
  // Store views never go stale by time: only the stream rereads them.
  const view = storeViewOptions("acme", scope, environmentSettingsQuery({ project: "web", environment: "production" }));
  expect(view.staleTime).toBe(Infinity);
  queryClient.setQueryData<unknown>(view.queryKey, { ok: true, value: { view: "environment" } });
  const replaced = vi.fn();
  const stop = watchOrganizationChanges("acme", ORG_A, scope, replaced);
  try {
    const first = FakeEventSource.latest;
    first?.dispatchEvent(new Event("open"));
    first?.dispatchEvent(new MessageEvent("organization", { data: JSON.stringify({ id: ORG_A }) }));
    vi.advanceTimersByTime(60_000);
    expect(replaced).not.toHaveBeenCalled();

    // A was removed: its stream ended, and the reconnect resolved the slug to B.
    first?.dispatchEvent(new Event("open"));
    first?.dispatchEvent(new MessageEvent("organization", { data: JSON.stringify({ id: ORG_B }) }));
    expect(replaced).toHaveBeenCalledOnce();
  } finally {
    stop();
    vi.useRealTimers();
    await getDbClient(queryClient).cleanup();
    queryClient.clear();
  }
});

it("rereads every change-log collection on every open", async () => {
  vi.stubGlobal("EventSource", FakeEventSource);
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  for (const table of orgStoreTableNames) queryClient.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed([]));
  const active = subscribeOrgStoreTables("acme", scope);
  await vi.waitFor(() => expect(queryClient.isFetching()).toBe(0));
  const fetched = new Set<unknown>();
  queryClient.getQueryCache().subscribe((event) => {
    if (event.type === "updated" && event.action.type === "fetch") fetched.add(event.query.queryKey.at(-1));
  });
  const stop = watchOrganizationChanges("acme", ORG_A, scope);
  try {
    FakeEventSource.latest?.dispatchEvent(new Event("open"));
    expect(fetched).toEqual(new Set(orgStoreTableNames));
    // A reconnect never resumes, so it rereads again.
    await vi.waitFor(() => expect(queryClient.isFetching()).toBe(0));
    fetched.clear();
    FakeEventSource.latest?.dispatchEvent(new Event("open"));
    expect(fetched).toEqual(new Set(orgStoreTableNames));
    await vi.waitFor(() => expect(queryClient.isFetching()).toBe(0));
  } finally {
    stop();
    for (const subscription of active) subscription.unsubscribe();
    await getDbClient(queryClient).cleanup();
    queryClient.clear();
  }
});

it("starts a fresh stream after a silent stretch, not while pings arrive", async () => {
  vi.stubGlobal("EventSource", FakeEventSource);
  vi.useFakeTimers({ toFake: ["setInterval", "clearInterval", "Date"] });
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  for (const get of Object.values(orgStoreTables)) vi.spyOn(get("acme", scope).utils, "refetch").mockResolvedValue([]);
  const stop = watchOrganizationChanges("acme", ORG_A, scope);
  try {
    const first = FakeEventSource.latest;
    vi.advanceTimersByTime(30_000);
    first?.dispatchEvent(new Event("ping"));
    vi.advanceTimersByTime(30_000);
    expect(FakeEventSource.latest).toBe(first);
    vi.advanceTimersByTime(30_000);
    expect(FakeEventSource.latest).not.toBe(first);
  } finally {
    stop();
    vi.useRealTimers();
    await getDbClient(queryClient).cleanup();
    queryClient.clear();
  }
});

it("refetches only the named collections and re-reads a renamed organization's state", async () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  for (const table of orgStoreTableNames) queryClient.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed([]));
  queryClient.setQueryData(organizationKeys.state("acme"), { name: "Acme" });
  const active = subscribeOrgStoreTables("acme", scope);
  await vi.waitFor(() => expect(queryClient.isFetching()).toBe(0));
  const fetched: unknown[] = [];
  queryClient.getQueryCache().subscribe((event) => {
    if (event.type === "updated" && event.action.type === "fetch") fetched.push(event.query.queryKey.at(-1));
  });

  applyOrganizationChanges(["environment_canvas_node_position", "organization"], "acme", scope);

  expect(fetched).toEqual(["environment_canvas_node_position"]);
  expect(queryClient.getQueryState(organizationKeys.state("acme"))?.isInvalidated).toBe(true);
  for (const subscription of active) subscription.unsubscribe();
});

it("rereads every Store view of the Organization when what support may see changes", () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(orgStoreTables.organization_support_access("acme", scope).utils, "refetch").mockResolvedValue([]);
  const view = ["store-view", "acme", "session", "user", { query: "environment" }];
  const other = ["store-view", "globex", "session", "user", { query: "environment" }];
  queryClient.setQueryData(view, { ok: true, value: {} });
  queryClient.setQueryData(other, { ok: true, value: {} });

  applyOrganizationChanges(["organization_support_access"], "acme", scope);

  expect(queryClient.getQueryState(view)?.isInvalidated).toBe(true);
  expect(queryClient.getQueryState(other)?.isInvalidated).toBe(false);
});
