import { afterEach, describe, expect, it, vi } from "vitest";
import { Effect } from "effect";
import {
  allocateUnique,
  getSlugWithSuffix,
  slugifySegment,
} from "#/utils/slug";

describe("slugifySegment", () => {
  it("lowercases, strips marks, and collapses punctuation", () => {
    expect(slugifySegment("  My Project  ")).toBe("my-project");
    expect(slugifySegment("Jerry Seinfeld")).toBe("jerry-seinfeld");
    expect(slugifySegment("Café")).toBe("cafe");
  });

  it("returns empty when nothing alphanumeric remains", () => {
    expect(slugifySegment("???")).toBe("");
    expect(slugifySegment("   ")).toBe("");
  });
});

describe("getSlugWithSuffix", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("returns the base slug on the first attempt", () => {
    expect(getSlugWithSuffix("api", 0)).toBe("api");
  });

  it("appends eight characters from a random UUID on later attempts", () => {
    vi.spyOn(globalThis.crypto, "randomUUID").mockReturnValue(
      "1b2d1234-zzzz-yyyy-xxxx-000000000000",
    );

    expect(getSlugWithSuffix("api", 1)).toBe("api-1b2d1234");
  });
});

describe("allocateUnique", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("returns the first successful insert using the base slug", async () => {
    const slugs: string[] = [];

    await expect(
      Effect.runPromise(
        allocateUnique({
          tryAttempt: (attempt) =>
            Effect.sync(() => {
              const slug = getSlugWithSuffix("api", attempt);
              slugs.push(slug);
              return { id: "project-1", slug };
            }),
          exhausted: new Error("exhausted"),
        }),
      ),
    ).resolves.toEqual({ id: "project-1", slug: "api" });

    expect(slugs).toEqual(["api"]);
  });

  it("retries with a suffix when the first insert collides", async () => {
    vi.spyOn(globalThis.crypto, "randomUUID").mockReturnValue(
      "1b2d1234-zzzz-yyyy-xxxx-000000000000",
    );
    const slugs: string[] = [];
    const results = [null, { id: "project-2" }];

    await expect(
      Effect.runPromise(
        allocateUnique({
          tryAttempt: (attempt) =>
            Effect.sync(() => {
              slugs.push(getSlugWithSuffix("api", attempt));
              return results.shift() ?? null;
            }),
          exhausted: new Error("exhausted"),
        }),
      ),
    ).resolves.toEqual({ id: "project-2" });

    expect(slugs).toEqual(["api", "api-1b2d1234"]);
  });

  it("fails when every attempt collides", async () => {
    await expect(
      Effect.runPromise(
        allocateUnique({
          tryAttempt: () => Effect.succeed(null),
          exhausted: new Error("Failed to create a unique project slug"),
          maxAttempts: 3,
        }),
      ),
    ).rejects.toThrow("Failed to create a unique project slug");
  });
});
