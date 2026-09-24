import { canonicalJson } from "./canonical-json";
import { destructivePublication, destructivePublicationMismatch } from "@ployz/sdk/config";
import type { CompiledSavedEnvironmentIntent } from "./saved-intent";
import { Schema } from "effect";
import { destructiveVolumeReviewsSchema } from "./destructive-volume-review";
import { Uuid } from "./workspace-schemas";
import { environmentSavedStateBasisSchema } from "./saved-state";


type ReviewedNodeSnapshot = {
  nodeType: "service" | "volume";
  nodeId: string;
  nodeLineageId: string;
  configVersion: number;
  config: unknown;
};

export type ReviewableNodeIdentity = Pick<
  ReviewedNodeSnapshot,
  "nodeType" | "nodeId"
> & {
  config?: unknown | null;
};

export type DestructiveEnvironmentSave = {
  serviceIds: string[];
  volumeIds: string[];
};

/**
 * Authority to publish one exact Working State revision as Saved State.
 *
 * The destructive set is required even when empty so every publisher is
 * checked against the same locked transition instead of opting into safety.
 */
export const reviewedEnvironmentPublicationSchema = Schema.Struct({
  savedStateBasis: environmentSavedStateBasisSchema,
  workingStateFingerprint: Schema.String.check(
    Schema.isPattern(/^environment-working-state-v1:[0-9a-f]{64}$/u),
  ),
  destructiveServiceIds: Schema.mutable(Schema.Array(Uuid)).check(
    Schema.makeFilter((serviceIds) =>
      new Set(serviceIds).size === serviceIds.length
        ? undefined
        : "A destructive Service can be reviewed only once.",
    ),
  ),
  destructiveVolumeReviews: destructiveVolumeReviewsSchema,
});
export type ReviewedEnvironmentPublication = typeof reviewedEnvironmentPublicationSchema.Type;

export function projectReviewedEnvironmentPublicationDestructiveSave(
  review: ReviewedEnvironmentPublication,
): DestructiveEnvironmentSave {
  return {
    serviceIds: review.destructiveServiceIds,
    volumeIds: review.destructiveVolumeReviews.map(
      (volumeReview) => volumeReview.target.resourceId,
    ),
  };
}

export const getDestructiveEnvironmentSaveReviewMismatch = destructivePublicationMismatch;
export function projectDestructiveEnvironmentSave(input: {
  workingNodes: ReviewableNodeIdentity[];
  appliedNodes: ReviewableNodeIdentity[];
}) {
  const runtimeNodes = (nodes: ReviewableNodeIdentity[]) => nodes.map((node) => ({
    nodeType: node.nodeType, nodeId: node.nodeId, config: node.config === null ? null : {},
  }));
  return destructivePublication({
    workingNodes: runtimeNodes(input.workingNodes),
    appliedNodes: runtimeNodes(input.appliedNodes),
  });
}

export type ReviewedEnvironmentWorkingState = {
  nodeSnapshots: ReviewedNodeSnapshot[];
  revisionMarkers?: string[];
};

/** The rendered document revision is the manual publication review boundary. */
export function projectReviewedEnvironmentWorkingState(document: {
  id: string; revision: string; compiled: CompiledSavedEnvironmentIntent;
}): ReviewedEnvironmentWorkingState {
  return { nodeSnapshots: document.compiled.nodeSnapshots,
    revisionMarkers: [`environment:${document.id}:${document.revision}`] };
}

export function canonicalReviewedEnvironmentWorkingStateJson(input: ReviewedEnvironmentWorkingState) {
  const order = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;
  return canonicalJson({
    nodeSnapshots: input.nodeSnapshots.map(({ nodeType, nodeId, nodeLineageId, configVersion, config }) =>
      ({ nodeType, nodeId, nodeLineageId, configVersion, config }))
      .sort((a, b) => order(`${a.nodeType}:${a.nodeId}`, `${b.nodeType}:${b.nodeId}`)),
    revisionMarkers: [...input.revisionMarkers ?? []].sort(order),
  }, ["encryptedValue", "parts"]);
}

export function formatReviewedEnvironmentWorkingStateFingerprint(
  digestHex: string,
): string {
  return `environment-working-state-v1:${digestHex}`;
}

export async function fingerprintReviewedEnvironmentWorkingState(
  input: ReviewedEnvironmentWorkingState,
): Promise<string> {
  const digest = await globalThis.crypto.subtle.digest(
    "SHA-256",
    new TextEncoder().encode(canonicalReviewedEnvironmentWorkingStateJson(input)),
  );
  return formatReviewedEnvironmentWorkingStateFingerprint(
    Array.from(new Uint8Array(digest), (byte) =>
      byte.toString(16).padStart(2, "0"),
    ).join(""),
  );
}
