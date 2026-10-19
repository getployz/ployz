import { expect, it } from "vitest";
import type { BrokenReference } from "@ployz/sdk";
import { brokenAt, brokenFiles, brokenVariables, brokenWhy } from "./store-broken";

const removed: BrokenReference = { service: "sentry", variable: "REDIS_URL", reads: "PLOYZ_PRIVATE_DOMAIN", missing: "service", from: "redis" };
const unset: BrokenReference = { service: "sentry", variable: "REDIS_URL", reads: "PASSWORD", missing: "variable", from: "redis" };
const file: BrokenReference = { config: "sentry", file: "config.yml", reads: "URL", missing: "service", from: null };

it("words why a reference is broken and where it is written", () => {
  expect(brokenWhy(removed)).toBe("redis was removed");
  expect(brokenWhy(unset)).toBe("redis has no PASSWORD");
  expect(brokenWhy(file)).toBe("Its Service was removed");
  expect(brokenAt(removed)).toBe("sentry · REDIS_URL");
  expect(brokenAt(file)).toBe("sentry · config.yml");
});

it("finds a Service's broken variables and a Config's broken files", () => {
  expect([...brokenVariables([removed, unset, file], "sentry")]).toEqual([["REDIS_URL", "redis was removed, redis has no PASSWORD"]]);
  expect([...brokenVariables([removed], "web")]).toEqual([]);
  expect([...brokenFiles([removed, file], "sentry")]).toEqual([["config.yml", "Its Service was removed"]]);
});
