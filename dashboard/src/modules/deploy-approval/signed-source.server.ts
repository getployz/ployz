import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { SavedRule } from "@ployz/sdk";
import { Option, Schema } from "effect";

const SourceRef = Schema.Struct({
  proposal: Schema.optional(Schema.Struct({ operation: Schema.String, index: Schema.Number })),
  services: Schema.Array(Schema.Struct({ name: Schema.String })),
});
const BundleSteps = Schema.Struct({
  operation: Schema.String,
  steps: Schema.Array(Schema.Struct({ index: Schema.Number, kind: Schema.String, digest: Schema.String })),
});
const decodeSource = Schema.decodeUnknownOption(Schema.fromJsonString(SourceRef));
const decodeBundle = Schema.decodeUnknownOption(Schema.fromJsonString(BundleSteps));

/**
 * The Services `saved` approves, when its body is the Source step its own bundle lists at the place the body names; null
 * when it isn't, so a rule changed after the tap is refused before the Store keeps it. Each Server checks the bundle's
 * signature itself: this only stops a Deploy whose rule they would all refuse.
 */
export function sourceServices({ rule }: SavedRule): ReadonlySet<string> | null {
  const body = Option.getOrNull(decodeSource(rule.body));
  const bundle = rule.proposal === undefined ? null : Option.getOrNull(decodeBundle(rule.proposal.body));
  if (body?.proposal === undefined || bundle === null || bundle.operation !== body.proposal.operation) return null;
  const step = bundle.steps[body.proposal.index];
  const digest = createHash("sha256").update(rule.body).digest("hex");
  return step?.kind === "source" && step.digest === digest ? new Set(body.services.map(({ name }) => name)) : null;
}
