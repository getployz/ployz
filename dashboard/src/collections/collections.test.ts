// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { changeNameSources } from "#/collections/change-sources";
import * as collections from "#/collections/collections";
import { orgStoreTables, getCanvasPositionsCollection } from "#/collections/collections";
import { orgStoreSeed, orgStoreTableNames } from "#/test/org-store-tables";

afterEach(() => {
  vi.useRealTimers();
});

describe("API node collections", () => {
  it("isolates node collections by authenticated scope while retaining automatic indexes", () => {
    const scope = { queryClient: new QueryClient(), sessionId: "session", userId: "user" };
    for (const get of [getCanvasPositionsCollection]) {
      const collection = get("acme", scope);
      expect(collection.config.autoIndex).toBe("eager");
      expect(get("acme", scope)).toBe(collection);
      expect(get("other-org", scope)).not.toBe(collection);
      expect(get("acme", { ...scope, sessionId: "other-session" })).not.toBe(collection);
      expect(get("acme", { ...scope, userId: "other-user" })).not.toBe(collection);
      expect(get("acme", { ...scope, queryClient: new QueryClient() })).not.toBe(collection);
    }
  });

  it("feeds every Org Store table from the change log and runs no timer", async () => {
    const tables = Object.entries(collections).filter(([name]) => /^get\w+Collection$/.test(name)).map(([, get]) => get);
    expect(new Set<unknown>(Object.values(orgStoreTables))).toEqual(new Set(tables));
    for (const name of orgStoreTableNames) expect(changeNameSources[name], name).not.toEqual([]);

    vi.useFakeTimers();
    const scope = { queryClient: new QueryClient(), sessionId: "session", userId: "user" };
    for (const table of orgStoreTableNames) scope.queryClient.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed([]));
    let reads = 0;
    scope.queryClient.getQueryCache().subscribe((event) => {
      if (event.type === "updated" && event.action.type === "fetch") reads += 1;
    });
    const active = Object.values(orgStoreTables).map((get) => get("acme", scope).subscribeChanges(() => {}));
    await vi.advanceTimersByTimeAsync(60_000);
    expect(reads).toBe(0);
    for (const subscription of active) subscription.unsubscribe();
  });
});
