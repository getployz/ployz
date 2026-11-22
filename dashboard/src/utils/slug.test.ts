import { describe, expect, it } from "vitest";
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
  it("returns the base slug on the first attempt", () => {
    expect(getSlugWithSuffix("api", 0)).toBe("api");
  });

  it("suffixes the base slug on later attempts", () => {
    const slug = getSlugWithSuffix("api", 1);

    expect(slug).toMatch(/^api-[a-z0-9]+$/);
    expect(slugifySegment(slug)).toBe(slug);
  });
});

describe("allocateUnique", () => {
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

    expect(slugs[0]).toBe("api");
    expect(slugs[1]).toMatch(/^api-[a-z0-9]+$/);
  });

  it("fails when every attempt collides", async () => {
    await expect(
      Effect.runPromise(
        allocateUnique({
          tryAttempt: () => Effect.succeed(null),
          exhausted: new Error("Failed to create a unique project slug"),
        }),
      ),
    ).rejects.toThrow("Failed to create a unique project slug");
  });
});
