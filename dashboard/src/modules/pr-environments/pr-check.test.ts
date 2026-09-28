import { describe, expect, it } from "vitest";
import { prCheck, type PrCheckDestination } from "./pr-check";

const production = (over: Partial<PrCheckDestination> = {}): PrCheckDestination => ({ name: "production", changes: 3, approval: null, ...over });
const approval = (over: Partial<NonNullable<PrCheckDestination["approval"]>> = {}) => ({
  standing: true, changes: 3, missing: [], approvedBy: "maya", ...over,
});

describe("prCheck", () => {
  it.each<[string, PrCheckDestination[], boolean, string]>([
    ["no Destination", [], true, "Nothing here deploys its target Git branch"],
    ["nothing to move", [production({ changes: 0 })], true, "Nothing here that production doesn’t have"],
    ["not approved", [production()], false, "Review and approve 3 changes for production"],
    ["one change, not approved", [production({ changes: 1 })], false, "Review and approve 1 change for production"],
    ["changed since approval", [production({ approval: approval({ standing: false }) })], false, "Changed since it was approved · review it again"],
    ["a missing value", [production({ approval: approval({ missing: ["STRIPE_KEY"] }) })], false, "STRIPE_KEY needs a value for production"],
    ["several missing values", [production({ approval: approval({ missing: ["STRIPE_KEY", "SENTRY_DSN"] }) })], false,
      "STRIPE_KEY, SENTRY_DSN need a value for production"],
    ["approved", [production({ approval: approval() })], true, "3 changes approved for production by maya"],
    ["approved, then nothing left to move", [production({ changes: 0, approval: approval({ changes: 2 }) })], true,
      "2 changes approved for production by maya"],
    ["two Destinations, neither approved", [production(), production({ name: "staging-eu", changes: 2 })], false,
      "Review and approve 5 changes for production and staging-eu"],
    ["two Destinations, one approved", [production({ approval: approval() }), production({ name: "staging-eu", changes: 2 })], false,
      "Review and approve 2 changes for staging-eu"],
    ["two Destinations, both approved", [production({ approval: approval() }), production({ name: "staging-eu", approval: approval({ changes: 2, approvedBy: "sam" }) })],
      true, "5 changes approved for production and staging-eu by maya and sam"],
    ["two Destinations, one with nothing to move", [production({ approval: approval() }), production({ name: "staging-eu", changes: 0 })], true,
      "3 changes approved for production by maya"],
  ])("%s", (_, destinations, passing, reason) => {
    expect(prCheck(destinations)).toEqual({ passing, reason });
  });
});
