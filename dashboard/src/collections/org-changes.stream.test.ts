// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { afterEach, expect, it, vi } from "vitest";
import { orgStoreTables } from "#/collections/collections";
import { applyOrganizationChanges, watchOrganizationChanges } from "#/collections/org-changes.stream";
import { getDbClient } from "#/collections/scope";
import { organizationKeys } from "#/modules/environment-design/workspace.queries";
import { environmentChangeStateOptions } from "#/modules/deployments/environment-change-state.queries";
import { orgStoreSeed, orgStoreTableNames } from "#/test/org-store-tables";

class FakeEventSource extends EventTarget {
  static latest: FakeEventSource | undefined;
  constructor() { super(); FakeEventSource.latest = this; }
  close() {}
}

afterEach(() => {
  vi.unstubAllGlobals();
});

it("refetches every change-log collection on every open", async () => {
  vi.stubGlobal("EventSource", FakeEventSource);
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const refetches = Object.values(orgStoreTables).map((get) => vi.spyOn(get("acme", scope).utils, "refetch").mockResolvedValue([]));
  const stop = watchOrganizationChanges("acme", scope);
  try {
    FakeEventSource.latest?.dispatchEvent(new Event("open"));
    for (const refetch of refetches) expect(refetch).toHaveBeenCalledOnce();
    // A reconnect never resumes, so it refetches again.
    FakeEventSource.latest?.dispatchEvent(new Event("open"));
    for (const refetch of refetches) expect(refetch).toHaveBeenCalledTimes(2);
  } finally {
    stop();
    await getDbClient(queryClient).cleanup();
    queryClient.clear();
  }
});

it("refetches only the named collections and re-reads a renamed organization's state", async () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  for (const table of orgStoreTableNames) queryClient.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed([]));
  queryClient.setQueryData(organizationKeys.state("acme"), { name: "Acme" });
  const active = Object.values(orgStoreTables).map((get) => get("acme", scope).subscribeChanges(() => {}));
  const fetched: unknown[] = [];
  queryClient.getQueryCache().subscribe((event) => {
    if (event.type === "updated" && event.action.type === "fetch") fetched.push(event.query.queryKey.at(-1));
  });

  applyOrganizationChanges(["environment_deployment", "organization"], "acme", scope);

  expect(fetched).toEqual(["environment_deployment"]);
  expect(queryClient.getQueryState(organizationKeys.state("acme"))?.isInvalidated).toBe(true);
  for (const subscription of active) subscription.unsubscribe();
});

it("re-reads the change-state projection, and no collection, when the change log names it", () => {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const { queryKey } = environmentChangeStateOptions("acme", scope);
  queryClient.setQueryData(queryKey, []);
  const refetches = Object.values(orgStoreTables).map((get) => vi.spyOn(get("acme", scope).utils, "refetch").mockResolvedValue([]));

  applyOrganizationChanges(["environment_change_state"], "acme", scope);

  expect(queryClient.getQueryState(queryKey)?.isInvalidated).toBe(true);
  for (const refetch of refetches) expect(refetch).not.toHaveBeenCalled();
});
