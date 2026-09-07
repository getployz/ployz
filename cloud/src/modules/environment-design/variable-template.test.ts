import { describe, expect, it } from "vitest";
import type { ValuePart } from "#/db/schema";
import {
  buildRefToken,
  caretToken,
  extractRefs,
  isPureLiteral,
  literalParts,
  parseDisplayToParts,
  partsToDisplay,
  partsToLiteralString,
  type LookupLineage,
  type LookupSlug,
} from "#/modules/environment-design/variable-template";

const DB_LINEAGE = "11111111-1111-1111-1111-111111111111";
const SET_LINEAGE = "22222222-2222-2222-2222-222222222222";

const lookupSlug: LookupSlug = (lineageId) => {
  if (lineageId === DB_LINEAGE) return "db";
  if (lineageId === SET_LINEAGE) return "shared";
  return null;
};

const lookupLineage: LookupLineage = (slug) => {
  if (slug === "db") return { lineageId: DB_LINEAGE, scope: "service" };
  if (slug === "shared") return { lineageId: SET_LINEAGE, scope: "variable_group" };
  return null;
};

describe("isPureLiteral / literalParts", () => {
  it("treats text-only parts as literal", () => {
    expect(isPureLiteral(literalParts("hello"))).toBe(true);
    expect(
      isPureLiteral([
        { kind: "text", value: "a" },
        { kind: "ref", owner: { scope: "self" }, key: "B" },
      ]),
    ).toBe(false);
  });
});

describe("partsToLiteralString", () => {
  it("returns the concatenated literal", () => {
    expect(partsToLiteralString(literalParts("postgres://host"))).toBe(
      "postgres://host",
    );
  });
  it("returns null when any ref is present", () => {
    expect(
      partsToLiteralString([
        { kind: "text", value: "x" },
        { kind: "ref", owner: { scope: "self" }, key: "Y" },
      ]),
    ).toBeNull();
  });
});

describe("partsToDisplay", () => {
  it("renders self, service, and variable group refs", () => {
    const parts: ValuePart[] = [
      { kind: "text", value: "postgres://u:" },
      { kind: "ref", owner: { scope: "service", lineageId: DB_LINEAGE }, key: "PASSWORD" },
      { kind: "text", value: "@" },
      { kind: "ref", owner: { scope: "service", lineageId: DB_LINEAGE }, key: "PLOYZ_PRIVATE_DOMAIN" },
      { kind: "text", value: "/app?key=" },
      { kind: "ref", owner: { scope: "self" }, key: "REGION" },
    ];
    expect(partsToDisplay(parts, lookupSlug)).toBe(
      "postgres://u:${{ db.PASSWORD }}@${{ db.PLOYZ_PRIVATE_DOMAIN }}/app?key=${{ REGION }}",
    );
  });

  it("renders a deleted producer with a sentinel", () => {
    const parts: ValuePart[] = [
      { kind: "ref", owner: { scope: "service", lineageId: "deadbeef" }, key: "X" },
    ];
    expect(partsToDisplay(parts, lookupSlug)).toBe("${{ <deleted>.X }}");
  });

  it("escapes a literal ${{ in text", () => {
    expect(partsToDisplay(literalParts("echo ${{not a ref}}"), lookupSlug)).toBe(
      "echo $${{not a ref}}",
    );
  });
});

describe("parseDisplayToParts", () => {
  it("round-trips with partsToDisplay", () => {
    const display =
      "postgres://u:${{ db.PASSWORD }}@${{ db.PLOYZ_PRIVATE_DOMAIN }}/app?r=${{ REGION }}&s=${{ shared.STRIPE }}";
    const { parts, unresolved } = parseDisplayToParts(display, lookupLineage);
    expect(unresolved).toEqual([]);
    expect(partsToDisplay(parts, lookupSlug)).toBe(display);
    expect(extractRefs(parts)).toEqual([
      { owner: { scope: "service", lineageId: DB_LINEAGE }, key: "PASSWORD" },
      { owner: { scope: "service", lineageId: DB_LINEAGE }, key: "PLOYZ_PRIVATE_DOMAIN" },
      { owner: { scope: "self" }, key: "REGION" },
      { owner: { scope: "variable_group", lineageId: SET_LINEAGE }, key: "STRIPE" },
    ]);
  });

  it("reports unresolved slugs and keeps them as literal text", () => {
    const { parts, unresolved } = parseDisplayToParts(
      "x=${{ ghost.Y }}",
      lookupLineage,
    );
    expect(unresolved).toEqual(["ghost"]);
    expect(isPureLiteral(parts)).toBe(true);
    expect(partsToLiteralString(parts)).toBe("x=${{ ghost.Y }}");
  });

  it("treats $${{ as a literal ${{", () => {
    const { parts } = parseDisplayToParts("echo $${{ FOO }}", lookupLineage);
    expect(isPureLiteral(parts)).toBe(true);
    expect(partsToLiteralString(parts)).toBe("echo ${{ FOO }}");
  });

  it("leaves a malformed ${{ as literal text", () => {
    const { parts } = parseDisplayToParts("${{ not-valid", lookupLineage);
    expect(isPureLiteral(parts)).toBe(true);
    expect(partsToLiteralString(parts)).toBe("${{ not-valid");
  });
});

describe("caretToken", () => {
  const at = (s: string) => caretToken(s, s.length);

  it("opens on `${{ `", () => {
    expect(at("url=${{ ")).toMatchObject({ ownerSlug: null, query: "" });
  });
  it("filters by partial key", () => {
    expect(at("url=${{ DAT")).toMatchObject({ ownerSlug: null, query: "DAT" });
  });
  it("captures owner slug after the dot", () => {
    expect(at("url=${{ db.PAS")).toMatchObject({ ownerSlug: "db", query: "PAS" });
    expect(at("url=${{ db.")).toMatchObject({ ownerSlug: "db", query: "" });
  });
  it("returns null outside a token and after a closed token", () => {
    expect(at("plain value")).toBeNull();
    expect(at("${{ db.X }} after")).toBeNull();
  });
  it("ignores an escaped $${{", () => {
    expect(at("a $${{ ")).toBeNull();
  });
  it("works across newlines (textarea)", () => {
    expect(at("A=1\nB=${{ api.")).toMatchObject({
      ownerSlug: "api",
      query: "",
    });
  });
  it("reports the replacement span", () => {
    const token = at("url=${{ db.PAS");
    expect(token?.start).toBe(4);
    expect(token?.end).toBe("url=${{ db.PAS".length);
  });
});

describe("buildRefToken", () => {
  it("builds self and owner tokens", () => {
    expect(buildRefToken({ ownerSlug: null, key: "REGION" })).toBe("${{ REGION }}");
    expect(buildRefToken({ ownerSlug: "db", key: "PASSWORD" })).toBe(
      "${{ db.PASSWORD }}",
    );
  });
});
