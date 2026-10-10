import type { Included, Readiness } from "@ployz/sdk";
import { Option, Schema } from "effect";

/**
 * Words for offers and pull request readiness, in one place: this copy is provisional, so each string lives here to
 * swap at once.
 */

/** The PR toolbar's Sync button once the Merge menu offered its changes. */
export const OFFERED = "Offered";

/** An offer row's action in Changes: moves it into the draft. */
export const INCLUDE = "Include";

/** A secret Include needs a value for. */
export const SET_VALUE = "Set value";

/** The Sync toast for a Merge-menu Sync. */
export const offeredTo = (into: string) => `Offered to ${into}`;

/** The Sync dialog's line for a Merge-menu Sync. */
export const includeAfterMerge = (into: string, number: number) => `${into} can include these after #${number} merges.`;

/** Why Save and Deploy wait: an included pull request that isn't ready. */
export const needs = (number: number) => `Needs #${number}`;

/** A pull request row's label in Changes. */
export function readinessLabel(readiness: Readiness, number: number) {
  switch (readiness) {
    case "open": return `Awaits #${number}`;
    case "ready": return "Ready";
    case "closed": return "Closed";
    case "elsewhere": return "Elsewhere";
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

/** The secrets an Include the Store refused with `conflict` needs a value for, by full name; null for any other refusal. */
export function needsValue(refusal: { code: string; details: unknown }): readonly string[] | null {
  if (refusal.code !== "conflict") return null;
  return Option.getOrNull(Option.map(decodeNeedsValue(refusal.details), ({ needs_value }) => needs_value));
}
