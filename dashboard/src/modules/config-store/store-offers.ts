import type { Included, Readiness } from "@ployz/sdk";
import { Option, Schema } from "effect";

/**
 * Words for offers and pull request readiness, in one place: this copy is provisional, so each string lives here to
 * swap at once.
 */

/** The PR toolbar's Sync button once the Merge menu queued its changes. */
export const OFFERED = "Queued";

/** An offer row's action in Changes: moves it into the draft. */
export const INCLUDE = "Apply";

/** A secret Apply needs a value for. */
export const SET_VALUE = "Set value";

/** The Sync toast for a Merge-menu Sync. */
export const offeredTo = (into: string) => `Queued for ${into}`;

/** The Sync dialog's line for a Merge-menu Sync: it starts the sentence, so the Destination's name does too. */
export const includeAfterMerge = (into: string, number: number) =>
  `${into.charAt(0).toUpperCase()}${into.slice(1)} can apply these once #${number} merges.`;

/** Why Save and Deploy wait: an included pull request that isn't ready. */
export const needs = (number: number) => `Waiting for #${number}`;

/** A pull request row's label in Changes; `mergedInto` names the branch it merged into, once it did. */
export function readinessLabel(readiness: Readiness, number: number, mergedInto?: string | null) {
  switch (readiness) {
    case "open": return needs(number);
    case "ready": return "Merged";
    case "closed": return "Closed";
    case "elsewhere": return mergedInto ? `Merged into ${mergedInto}` : "Merged elsewhere";
  }
}

/** The first included pull request Save and Deploy wait on: in the draft, not ready. */
export function blockingPullRequest(included: readonly Included[]): number | null {
  for (const item of included) {
    if (!item.offered && item.source.kind === "pull_request" && item.readiness !== undefined && item.readiness !== "ready") return item.source.number;
  }
  return null;
}

const decodeNeedsValue = Schema.decodeUnknownOption(Schema.Struct({ needs_value: Schema.Array(Schema.String) }));

/** The secrets an Apply the Store refused with `conflict` needs a value for, by full name; null for any other refusal. */
export function needsValue(refusal: { code: string; details: unknown }): readonly string[] | null {
  if (refusal.code !== "conflict") return null;
  return Option.getOrNull(Option.map(decodeNeedsValue(refusal.details), ({ needs_value }) => needs_value));
}
