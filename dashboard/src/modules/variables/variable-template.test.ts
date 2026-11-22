import { describe, expect, it } from "vitest";
import {
  buildRefToken,
  caretToken,
  parseDisplayToParts,
  referencesDeletedOwner,
  renameReferences,
} from "./variable-template";

const known = (name: string) => name === "db" || name === "shared";

describe("parseDisplayToParts", () => {
  it("reports unknown names and keeps their tokens as literal text", () => {
    const { parts, unresolved } = parseDisplayToParts("x=${{ ghost.Y }} ${{ db.Z }}", known);
    expect(unresolved).toEqual(["ghost"]);
    expect(parts).toEqual([
      { kind: "text", value: "x=${{ ghost.Y }} " },
      { kind: "ref", owner: "db", key: "Z" },
    ]);
  });

  it("treats $${{ and a malformed ${{ as literal text", () => {
    expect(parseDisplayToParts("echo $${{ FOO }}", known).parts).toEqual([{ kind: "text", value: "echo ${{ FOO }}" }]);
    expect(parseDisplayToParts("${{ not-valid", known).parts).toEqual([{ kind: "text", value: "${{ not-valid" }]);
  });

  it("reads a removed service's token, named or not, as the Store shows it", () => {
    const display =
      "${{ <deleted redis-2>.URL }},${{ <deleted redis-2#2>.URL }},${{ <deleted>.URL }},${{ <deleted #3f2a9c01>.URL }},${{ db.URL }}";
    const { unresolved } = parseDisplayToParts(display, known);
    expect(unresolved).toEqual(["<deleted redis-2>", "<deleted redis-2#2>", "<deleted>", "<deleted #3f2a9c01>"]);
    expect(renameReferences(display, "api", "db", "URL", "DSN")).toBe(
      "${{ <deleted redis-2>.URL }},${{ <deleted redis-2#2>.URL }},${{ <deleted>.URL }},${{ <deleted #3f2a9c01>.URL }},${{ db.DSN }}",
    );
    expect(referencesDeletedOwner("${{ <deleted #3f2a9c01>.URL }}")).toBe(true);
    expect(referencesDeletedOwner("${{ <deleted #3>.URL }}")).toBe(false);
    expect(referencesDeletedOwner("${{ <deleted redis#0>.URL }}")).toBe(false);
    expect(referencesDeletedOwner("${{ <deleted redis-2>.URL }}")).toBe(true);
    expect(referencesDeletedOwner("${{<deleted>.URL}}")).toBe(true);
    expect(referencesDeletedOwner("$${{ <deleted redis>.URL }}")).toBe(false);
  });
});

describe("renameReferences", () => {
  it("renames references to the key of the service, own or by name, and keeps escaped text", () => {
    expect(renameReferences("a=${{ db.URL }} b=${{ URL }} c=${{ web.URL }} d=$${{ db.URL }}", "db", "db", "URL", "DSN"))
      .toBe("a=${{ db.DSN }} b=${{ DSN }} c=${{ web.URL }} d=$${{ db.URL }}");
  });

  it("returns null when nothing references the key", () => {
    expect(renameReferences("${{ web.URL }}", "api", "db", "URL", "DSN")).toBeNull();
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
