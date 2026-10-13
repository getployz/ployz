import { describe, expect, it } from "vitest";
import { notSetToast, sealedOnlyPaths } from "./history-not-set";

describe("notSetToast", () => {
  it("names every key that came back Not set", () => {
    expect(notSetToast("undo", [{ service: "billing", key: "STRIPE_KEY" }, { service: "billing", key: "WEBHOOK_SECRET" }])).toEqual({
      title: "Undo staged",
      description: "STRIPE_KEY and WEBHOOK_SECRET came back Not set. Deploys leave them out until you set them.",
    });
  });

  it("speaks of one key in the singular", () => {
    expect(notSetToast("restore", [{ service: "web", key: "TOKEN" }])).toEqual({
      title: "Roll back staged",
      description: "TOKEN came back Not set. Deploys leave it out until you set it.",
    });
  });

  it("names a key two Services share once", () => {
    expect(notSetToast("undo", [{ service: "api", key: "TOKEN" }, { service: "web", key: "TOKEN" }])?.description)
      .toBe("TOKEN came back Not set. Deploys leave it out until you set it.");
  });

  it("says nothing when every key kept its value", () => {
    expect(notSetToast("undo", [])).toBeUndefined();
  });
});

describe("sealedOnlyPaths", () => {
  it("holds the variables a save sealed, which History never undoes", () => {
    expect(sealedOnlyPaths([{ settings: [
      { path: "billing.env.STRIPE_KEY", before: "plain", after: { secret: true } },
      { path: "billing.env.TOKEN", before: { secret: true }, after: null },
      { path: "billing.env.PLAIN", before: "a", after: "b" },
      { path: "billing.replicas", before: 1, after: 2 },
    ] }], {})).toEqual(new Set(["billing.env.STRIPE_KEY"]));
  });

  it("leaves out a renamed secret, whose Undo puts the old name back", () => {
    const rename = [{ settings: [
      { path: "billing.env.STRIPE_KEY", before: { secret: true }, after: null },
      { path: "billing.env.STRIPE_SECRET", before: null, after: { secret: true } },
    ] }];
    expect(sealedOnlyPaths(rename, { "billing.env.STRIPE_KEY": 2, "billing.env.STRIPE_SECRET": 2 })).toEqual(new Set());
  });
});
