import { describe, expect, it } from "vitest";
import { getRawServicesCollection } from "#/electric/collections";
import { tableSyncUrl } from "#/electric/table-sync-url";

const TEST_BASE_URL = "http://localhost:61859/proxied/cloud/nick";

describe("Electric collection table-sync proxy URLs", () => {
  it("builds an absolute organization table-sync URL", () => {
    const value = tableSyncUrl(
      "environment_deployment",
      { organizationSlug: "nick" },
      TEST_BASE_URL,
    );

    expect(() => new URL(value)).not.toThrow();
    const url = new URL(value);
    expect(url.origin).toBe("http://localhost:61859");
    expect(url.pathname).toBe("/api/shapes/environment_deployment");
    expect(url.searchParams.get("organizationSlug")).toBe("nick");
  });

  it("builds an absolute user table-sync URL", () => {
    const url = new URL(tableSyncUrl("github_repository_cache", {}, TEST_BASE_URL));

    expect(url.origin).toBe("http://localhost:61859");
    expect(url.pathname).toBe("/api/shapes/github_repository_cache");
  });

  it("enables automatic indexes for client-side joins", () => {
    const collection = getRawServicesCollection(
      "auto-index-test",
      TEST_BASE_URL,
    );

    expect(collection.config.autoIndex).toBe("eager");
  });
});
