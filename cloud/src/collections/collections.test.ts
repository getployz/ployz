import { QueryClient } from "@tanstack/react-query";
import { describe, expect, it } from "vitest";
import { getRawServicesCollection, getRawEnvironmentResourcesCollection, getCanvasPositionsCollection, getResourceLineagesCollection } from "#/collections/collections";

describe("API node collections", () => {
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
