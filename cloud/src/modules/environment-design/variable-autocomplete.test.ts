import { describe, expect, it } from "vitest";
import {
  buildReferenceTargets,
  filterReferenceTargets,
  type ReferenceTarget,
} from "#/modules/environment-design/variable-autocomplete";

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
const shared = {
  slug: "shared",
  name: "Shared",
  isSelf: false,
  variables: [{ key: "STRIPE", exported: true, isSecret: true, description: null }],
};

describe("buildReferenceTargets", () => {
  it("offers self vars, self managed exports, other services' exports, and variable group exports", () => {
    const targets = buildReferenceTargets({
      ownerScope: "service",
      services: [api, db],
      variableGroups: [shared],
    });
    const labels = targets.map((t) => `${t.ownerSlug ?? ""}.${t.key}`);
    expect(labels).toContain(".REGION"); // self, no prefix
    expect(labels).toContain(".INTERNAL"); // self non-exported still offered
    expect(labels).toContain(".PLOYZ_PRIVATE_DOMAIN"); // self managed export
    expect(labels).toContain("db.PASSWORD"); // other service exported
    expect(labels).toContain("db.PLOYZ_PRIVATE_DOMAIN"); // other service managed
    expect(labels).toContain("shared.STRIPE"); // variable group exported
    expect(labels).not.toContain("db.INTERNAL_ONLY"); // other service non-exported hidden
  });

  it("offers only the group's own variables for a variable-group owner", () => {
    const targets = buildReferenceTargets({
      ownerScope: "variable_group",
      services: [api, db],
      variableGroups: [{ ...shared, isSelf: true }],
    });
    expect(targets).toEqual([
      {
        key: "STRIPE",
        ownerSlug: null,
        kind: "self",
        ownerLabel: "This group",
        isSecret: true,
        description: null,
      },
    ]);
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
});
