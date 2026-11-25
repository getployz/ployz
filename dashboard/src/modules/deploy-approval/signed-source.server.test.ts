import type { SavedRule } from "@ployz/sdk";
import { versionSteps } from "@ployz/sdk/authority";
import { describe, expect, it } from "vitest";
import { CLUSTER, view } from "./rule-view.test-fixture";
import { sourceServices } from "./signed-source.server";

/** One tap's bundle and the Source step it drafts for web, the repository owned by `owner`. */
const tap = (owner: number) => {
  const { sources, target } = view();
  const draft = versionSteps({
    cluster: CLUSTER, namespace: "app", services: [], operation: "4".repeat(32), issued_at: 1_000, expires_at: 601_000,
    seed: Array.from({ length: 16 }, () => 3), against: { trusted: { devices: [], passkeys: [], grants: [] }, containers: [] },
    source: { rule: null, sources, target, machines: ["1".repeat(32)], owner_id: owner },
  });
  if (draft?.source === undefined) throw new Error("a first Deploy signs a rule");
  return { bundle: draft.bundle, body: draft.source.body };
};

/** `body` carried under `bundle`, as a decision posts the rule its passkey signed. */
const saved = (body: string, bundle: string): SavedRule => ({ rule: { body, signatures: [], proposal: { body: bundle, signatures: [] } }, births: {}, prior: null });

describe("sourceServices", () => {
  it("names the Services the tap's own Source step approves", () => {
    const { bundle, body } = tap(7);

    expect(sourceServices(saved(body, bundle))).toEqual(new Set(["web"]));
  });

  // Catches a decision whose rule was swapped after the tap: the Store would keep a rule every Server refuses.
  it("refuses a Source step the bundle doesn't list", () => {
    const { bundle } = tap(7);
    const swapped = tap(8).body;

    expect(sourceServices(saved(swapped, bundle))).toBeNull();
  });
});
