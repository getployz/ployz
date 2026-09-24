import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import { getRawServicesCollection, getRawEnvironmentResourcesCollection, getCanvasPositionsCollection, getResourceLineagesCollection } from "#/collections/collections";
import { dataSources } from "#/collections/data-sources";
import { orgStoreProjections, orgStoreViews } from "#/collections/org-store";

describe("API node collections", () => {
  it("gates on every view and projection registered as Org Store", () => {
    const modules = import.meta.glob<object>(["/src/modules/**/*.collection.ts", "/src/modules/**/*.queries.ts"], { eager: true });
    const exported = (suffix: string, name: RegExp) => Object.entries(dataSources)
      .filter(([path, source]) => source.kind === "org-store" && path.endsWith(suffix))
      .flatMap(([path]) => Object.entries(modules[`/src/${path}`] ?? {}).filter(([key]) => name.test(key)).map(([, value]) => value));
    const views = exported(".collection.ts", /^get\w+Collection$/);
    const projections = exported(".queries.ts", /^preload\w+$/);
    expect(views.length).toBeGreaterThan(0);
    expect(projections.length).toBeGreaterThan(0);
    expect(new Set<unknown>(orgStoreViews)).toEqual(new Set(views));
    expect(new Set<unknown>(orgStoreProjections)).toEqual(new Set(projections));
  });

  it("isolates node collections by authenticated scope while retaining automatic indexes", () => {
    const scope = { queryClient: new QueryClient(), sessionId: "session", userId: "user" };
    for (const get of [getRawServicesCollection, getRawEnvironmentResourcesCollection,
      getCanvasPositionsCollection, getResourceLineagesCollection]) {
      const collection = get("acme", scope);
      expect(collection.config.autoIndex).toBe("eager");
      expect(get("acme", scope)).toBe(collection);
      expect(get("other-org", scope)).not.toBe(collection);
      expect(get("acme", { ...scope, sessionId: "other-session" })).not.toBe(collection);
      expect(get("acme", { ...scope, userId: "other-user" })).not.toBe(collection);
      expect(get("acme", { ...scope, queryClient: new QueryClient() })).not.toBe(collection);
    }
  });
});
