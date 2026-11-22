// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { afterEach, describe, expect, it, vi } from "vitest";
import { sourcesOf } from "#/collections/change-sources";
import { getCanvasPositionsCollection } from "#/collections/collections";
import { orgStoreSeed, orgStoreTableNames, subscribeOrgStoreTables } from "#/test/org-store-tables";

afterEach(() => {
  vi.useRealTimers();
});

describe("API node collections", () => {
  it("isolates node collections by authenticated scope", () => {
    const scope = { queryClient: new QueryClient(), sessionId: "session", userId: "user" };
    const collection = getCanvasPositionsCollection("acme", scope);
    expect(getCanvasPositionsCollection("acme", scope)).toBe(collection);
    expect(getCanvasPositionsCollection("other-org", scope)).not.toBe(collection);
    expect(getCanvasPositionsCollection("acme", { ...scope, sessionId: "other-session" })).not.toBe(collection);
    expect(getCanvasPositionsCollection("acme", { ...scope, userId: "other-user" })).not.toBe(collection);
    expect(getCanvasPositionsCollection("acme", { ...scope, queryClient: new QueryClient() })).not.toBe(collection);
  });

  it("feeds every Org Store table from the change log and runs no timer", async () => {
    for (const name of orgStoreTableNames) expect(sourcesOf(name), name).not.toEqual([]);

    vi.useFakeTimers();
    const scope = { queryClient: new QueryClient(), sessionId: "session", userId: "user" };
    for (const table of orgStoreTableNames) scope.queryClient.setQueryData(["collections", "session", "user", "acme", table], orgStoreSeed([]));
    let reads = 0;
    scope.queryClient.getQueryCache().subscribe((event) => {
      if (event.type === "updated" && event.action.type === "fetch") reads += 1;
    });
    const active = subscribeOrgStoreTables("acme", scope);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(reads).toBe(0);
    for (const subscription of active) subscription.unsubscribe();
  });
});
