import { expect, it } from "vitest";
import { clusterNameProblem, clusterNameView, defaultClusterName } from "#/modules/cluster-name/cluster-name";

it("defaults to the slug cut to one DNS label, never ending on a hyphen", () => {
  expect(defaultClusterName("acme")).toBe("acme");
  expect(defaultClusterName("a".repeat(70))).toBe("a".repeat(63));
  expect(defaultClusterName(`${"a".repeat(62)}-b`)).toBe("a".repeat(62));
});

it("names the rule a name breaks", () => {
  expect(clusterNameProblem("acme-2")).toBeNull();
  expect(clusterNameProblem("a".repeat(64))).toBe("63 characters at most.");
  for (const bad of ["Acme", "-acme", "acme-", "a_b", ""]) expect(clusterNameProblem(bad)).not.toBeNull();
});

it("shows a name pending until a Server confirms it, and the default before founding", () => {
  const row = { id: "o", name: "acme", appliedName: null, refusal: null };
  expect(clusterNameView(undefined, "shop", false)).toMatchObject({ state: "current", name: "shop" });
  expect(clusterNameView(undefined, "shop", true)).toMatchObject({ state: "unset", name: null });
  expect(clusterNameView(row, "shop", false)).toMatchObject({ state: "current", name: "acme" });
  expect(clusterNameView(row, "shop", true)).toMatchObject({ state: "pending", name: "acme" });
  expect(clusterNameView({ ...row, appliedName: "acme" }, "shop", true)).toMatchObject({ state: "current" });
  expect(clusterNameView({ ...row, refusal: "Needs a newer Ployz on your servers." }, "shop", true)).toMatchObject({ state: "refused", refusal: "Needs a newer Ployz on your servers." });
});
