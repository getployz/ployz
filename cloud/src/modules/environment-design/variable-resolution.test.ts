import { Result } from "effect";
import { describe, expect, it } from "vitest";
import type { ValuePart } from "#/db/schema";
import {
  resolveValueParts,
  type ProducerLookup,
  type ResolverProducer,
} from "#/modules/environment-design/variable-resolution";

const text = (value: string): ValuePart => ({ kind: "text", value });
const selfRef = (key: string): ValuePart => ({
  kind: "ref",
  owner: { scope: "self" },
  key,
});
const svcRef = (lineageId: string, key: string): ValuePart => ({
  kind: "ref",
  owner: { scope: "service", lineageId },
  key,
});
const variableGroupRef = (lineageId: string, key: string): ValuePart => ({
  kind: "ref",
  owner: { scope: "variable_group", lineageId },
  key,
});

type Node = { ownerId: string; key: string; producer: ResolverProducer };

/** Build a ProducerLookup over a fixed set of producers + lineage→ownerId map. */
function makeLookup(
  nodes: Node[],
  lineageToOwner: Record<string, string> = {},
): ProducerLookup {
  const map = new Map(nodes.map((node) => [`${node.ownerId}::${node.key}`, node]));
  return ({ owner, selfOwnerId, key }) => {
    const ownerId =
      owner.scope === "self" ? selfOwnerId : lineageToOwner[owner.lineageId];
    if (!ownerId) return null;
    const node = map.get(`${ownerId}::${key}`);
    return node ? { ownerId, producer: node.producer } : null;
  };
}

function unwrap(result: ReturnType<typeof resolveValueParts>) {
  if (Result.isFailure(result)) {
    throw new Error(`expected ok, got error: ${result.failure.message}`);
  }
  return result.success;
}

describe("resolveValueParts", () => {
  it("returns a pure literal unchanged", () => {
    const out = unwrap(
      resolveValueParts({
        parts: [text("postgres://host")],
        selfOwnerId: "svc1",
        lookup: makeLookup([]),
      }),
    );
    expect(out).toEqual({ value: "postgres://host", secret: false, warnings: [] });
  });

  it("interpolates a self ref to a literal producer", () => {
    const out = unwrap(
      resolveValueParts({
        parts: [text("region="), selfRef("REGION")],
        selfOwnerId: "svc1",
        lookup: makeLookup([
          { ownerId: "svc1", key: "REGION", producer: { kind: "literal", value: "us-east" } },
        ]),
      }),
    );
    expect(out.value).toBe("region=us-east");
    expect(out.secret).toBe(false);
  });

  it("interpolates cross-service, variable group, and managed-style refs by lineageId", () => {
    const out = unwrap(
      resolveValueParts({
        parts: [
          text("postgres://u:"),
          svcRef("db-lineage", "PASSWORD"),
          text("@"),
          svcRef("db-lineage", "PLOYZ_PRIVATE_DOMAIN"),
          text("/app?s="),
          variableGroupRef("group-lineage", "STRIPE"),
        ],
        selfOwnerId: "web",
        lookup: makeLookup(
          [
            { ownerId: "db", key: "PASSWORD", producer: { kind: "literal", value: "pw" } },
            {
              ownerId: "db",
              key: "PLOYZ_PRIVATE_DOMAIN",
              producer: { kind: "literal", value: "db-prod.internal" },
            },
            { ownerId: "shared", key: "STRIPE", producer: { kind: "literal", value: "sk_live" } },
          ],
          { "db-lineage": "db", "group-lineage": "shared" },
        ),
      }),
    );
    expect(out.value).toBe("postgres://u:pw@db-prod.internal/app?s=sk_live");
    expect(out.secret).toBe(false);
  });

  it("propagates secret-ness when any (transitive) part is a secret producer", () => {
    const out = unwrap(
      resolveValueParts({
        parts: [text("postgres://u:"), svcRef("db-lineage", "PASSWORD"), text("@host")],
        selfOwnerId: "web",
        lookup: makeLookup(
          [{ ownerId: "db", key: "PASSWORD", producer: { kind: "secret", value: "s3cret" } }],
          { "db-lineage": "db" },
        ),
      }),
    );
    expect(out.value).toBe("postgres://u:s3cret@host");
    expect(out.secret).toBe(true);
  });

  it("recurses through a templated producer and inherits its secret-ness", () => {
    // db.DATABASE_URL is itself a template embedding the sealed db.PASSWORD.
    const out = unwrap(
      resolveValueParts({
        parts: [selfRef("DATABASE_URL")],
        selfOwnerId: "db",
        lookup: makeLookup([
          {
            ownerId: "db",
            key: "DATABASE_URL",
            producer: { kind: "template", parts: [text("postgres://u:"), selfRef("PASSWORD")] },
          },
          { ownerId: "db", key: "PASSWORD", producer: { kind: "secret", value: "pw" } },
        ]),
      }),
    );
    expect(out.value).toBe("postgres://u:pw");
    expect(out.secret).toBe(true);
  });

  it("resolves a missing ref to empty string and records a warning", () => {
    const out = unwrap(
      resolveValueParts({
        parts: [text("x="), svcRef("ghost-lineage", "Y"), selfRef("Z")],
        selfOwnerId: "svc1",
        lookup: makeLookup([]),
      }),
    );
    expect(out.value).toBe("x=");
    expect(out.warnings).toEqual([
      { kind: "missing", ownerId: null, key: "Y" },
      { kind: "missing", ownerId: "svc1", key: "Z" },
    ]);
  });

  it("detects a direct two-node cycle A→B→A", () => {
    const result = resolveValueParts({
      parts: [selfRef("B")],
      selfOwnerId: "svc1",
      lookup: makeLookup([
        { ownerId: "svc1", key: "A", producer: { kind: "template", parts: [selfRef("B")] } },
        { ownerId: "svc1", key: "B", producer: { kind: "template", parts: [selfRef("A")] } },
      ]),
    });
    expect(Result.isFailure(result)).toBe(true);
    if (Result.isFailure(result)) {
      expect(result.failure._tag).toBe("TemplateResolveError");
      expect(result.failure.code).toBe("cycle");
      expect(result.failure.path).toContain("svc1::B");
    }
  });

  it("detects a longer cycle A→B→C→A", () => {
    const result = resolveValueParts({
      parts: [selfRef("B")],
      selfOwnerId: "svc1",
      lookup: makeLookup([
        { ownerId: "svc1", key: "B", producer: { kind: "template", parts: [selfRef("C")] } },
        { ownerId: "svc1", key: "C", producer: { kind: "template", parts: [selfRef("B")] } },
      ]),
    });
    expect(Result.isFailure(result)).toBe(true);
    if (Result.isFailure(result)) {
      expect(result.failure.code).toBe("cycle");
    }
  });

  it("reuses a producer referenced twice (diamond) without flagging a cycle", () => {
    const out = unwrap(
      resolveValueParts({
        parts: [selfRef("LEFT"), text("|"), selfRef("RIGHT")],
        selfOwnerId: "svc1",
        lookup: makeLookup([
          { ownerId: "svc1", key: "LEFT", producer: { kind: "template", parts: [selfRef("BASE")] } },
          { ownerId: "svc1", key: "RIGHT", producer: { kind: "template", parts: [selfRef("BASE")] } },
          { ownerId: "svc1", key: "BASE", producer: { kind: "literal", value: "b" } },
        ]),
      }),
    );
    expect(out.value).toBe("b|b");
    expect(out.secret).toBe(false);
  });
});
