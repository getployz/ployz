import { describe, expect, it } from "vitest";
import {
  buildReferenceTargets,
  filterReferenceTargets,
  type ReferenceTarget,
} from "./variable-autocomplete";
import { getManagedServiceExports } from "./managed-service-exports";

const api = {
  slug: "api",
  name: "API",
  isSelf: true,
  variables: [
    { key: "REGION", exported: false, isSecret: false, description: null },
    { key: "INTERNAL", exported: false, isSecret: false, description: null },
  ],
  managedExports: [{ key: "PLOYZ_PRIVATE_DOMAIN", description: "Private DNS." }],
};
const db = {
  slug: "db",
  name: "Postgres",
  isSelf: false,
  variables: [
    { key: "PASSWORD", exported: true, isSecret: true, description: null },
    { key: "INTERNAL_ONLY", exported: false, isSecret: false, description: null },
  ],
  managedExports: [{ key: "PLOYZ_PRIVATE_DOMAIN", description: "Private DNS." }],
};

describe("buildReferenceTargets", () => {
  it("offers self vars, self managed exports, and other services' exports", () => {
    const targets = buildReferenceTargets({ services: [api, db] });
    const labels = targets.map((t) => `${t.ownerSlug ?? ""}.${t.key}`);
    expect(labels).toContain(".REGION"); // self, no prefix
    expect(labels).toContain(".INTERNAL"); // self non-exported still offered
    expect(labels).toContain(".PLOYZ_PRIVATE_DOMAIN"); // self managed export
    expect(labels).toContain("db.PASSWORD"); // other service exported
    expect(labels).toContain("db.PLOYZ_PRIVATE_DOMAIN"); // other service managed
    expect(labels).not.toContain("db.INTERNAL_ONLY"); // other service non-exported hidden
  });
});

describe("filterReferenceTargets", () => {
  const targets: ReferenceTarget[] = [
    { key: "REGION", ownerSlug: null, kind: "self", ownerLabel: "x", isSecret: false, description: null },
    { key: "PASSWORD", ownerSlug: "db", kind: "service", ownerLabel: "db", isSecret: true, description: null },
    { key: "PORT", ownerSlug: "db", kind: "managed", ownerLabel: "db", isSecret: false, description: null },
  ];

  it("matches self keys and owner slugs when no owner is typed", () => {
    const result = filterReferenceTargets(targets, {
      start: 0,
      end: 0,
      query: "d",
      ownerSlug: null,
    });
    expect([...result.map((t) => t.key)].sort()).toEqual(["PASSWORD", "PORT"]); // both under "db"
  });

  it("restricts to the owner's keys once a slug is typed", () => {
    const result = filterReferenceTargets(targets, {
      start: 0,
      end: 0,
      query: "PASS",
      ownerSlug: "db",
    });
    expect(result.map((t) => t.key)).toEqual(["PASSWORD"]);
  });

  it("offers a typed owner's managed exports in definition order, private domain first", () => {
    const redis = {
      slug: "redis", name: "redis", isSelf: false,
      variables: [{ key: "AUTH", exported: true, isSecret: true, description: null }],
      managedExports: getManagedServiceExports({
        id: "s1", lineageId: "s1", name: "redis", slug: "redis", privateDns: "redis",
        environmentId: "e1", environmentSlug: "production",
      }),
    };
    const result = filterReferenceTargets(buildReferenceTargets({ services: [api, redis] }), {
      start: 0, end: 0, query: "red", ownerSlug: null,
    });
    expect(result.map((t) => `${t.ownerSlug}.${t.key}`)).toEqual([
      "redis.PLOYZ_PRIVATE_DOMAIN",
      "redis.PORT",
      "redis.PLOYZ_ENVIRONMENT_NAME",
      "redis.PLOYZ_SERVICE_NAME",
      "redis.PLOYZ_ENVIRONMENT_ID",
      "redis.PLOYZ_SERVICE_ID",
      "redis.AUTH",
    ]);
  });

  it("matches by prefix only, never fuzzily", () => {
    const result = filterReferenceTargets(targets, { start: 0, end: 0, query: "ASS", ownerSlug: "db" });
    expect(result).toEqual([]);
  });

  it("sorts an owner's own variables by key", () => {
    const owned: ReferenceTarget[] = ["ZED", "ALPHA", "MID"].map((key) => (
      { key, ownerSlug: "db", kind: "service", ownerLabel: "db", isSecret: false, description: null }
    ));
    const result = filterReferenceTargets(owned, { start: 0, end: 0, query: "", ownerSlug: "db" });
    expect(result.map((t) => t.key)).toEqual(["ALPHA", "MID", "ZED"]);
  });
});
