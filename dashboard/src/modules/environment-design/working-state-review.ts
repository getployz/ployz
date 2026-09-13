import { canonicalWorkingReview, destructivePublication, destructivePublicationMismatch } from "@ployz/sdk/config";
import type { CompiledEnvironmentIntent } from "@ployz/sdk/config";
import type { DestructiveVolumeReview } from "./destructive-volume-review";
import type { EnvironmentSavedStateBasis } from "./saved-state";

type ReviewedNodeSnapshot = {
  nodeType: "service" | "variable_group" | "volume";
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
export type ReviewedEnvironmentPublication = {
  savedStateBasis: EnvironmentSavedStateBasis;
  workingStateFingerprint: string;
  destructiveServiceIds: string[];
  destructiveVolumeReviews: DestructiveVolumeReview[];
};

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
export const projectDestructiveEnvironmentSave = destructivePublication;

export type ReviewedEnvironmentWorkingState = {
  nodeSnapshots: ReviewedNodeSnapshot[];
  revisionMarkers?: string[];
};

/** The rendered document revision is the manual publication review boundary. */
export function projectReviewedEnvironmentWorkingState(document: {
  id: string; revision: string; compiled: CompiledEnvironmentIntent;
}): ReviewedEnvironmentWorkingState {
  return { nodeSnapshots: document.compiled.nodeSnapshots,
    revisionMarkers: [`environment:${document.id}:${document.revision}`] };
}

export const canonicalReviewedEnvironmentWorkingStateJson = canonicalWorkingReview;

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
