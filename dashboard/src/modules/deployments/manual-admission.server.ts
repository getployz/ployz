import "@tanstack/react-start/server-only";

import { Effect } from "effect";
import { loadCurrentEnvironmentState } from "#/modules/environment-design/working-state-repository.server";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { fingerprintReviewedEnvironmentWorkingStateSync } from "#/modules/environment-design/working-state-fingerprint.server";
import type { ReviewedEnvironmentPublication } from "#/modules/environment-design/working-state-review";
import { publishEnvironmentSavedState } from "#/modules/environment-design/saved-state-operations.server";
import { admitEnvironmentDeployment } from "#/modules/deployments/admission.server";
import { Conflict } from "#/server/public-error";

export const createManualEnvironmentDeployment = Effect.fn(
  "Deployments.createManualEnvironmentDeployment",
)(function* (input: {
  readonly environmentId: string;
  readonly actorId: string;
  readonly message: string | null;
  readonly review: ReviewedEnvironmentPublication;
}) {
  yield* lockEnvironmentDeploymentQueue(input.environmentId);
  const state = yield* loadCurrentEnvironmentState(input.environmentId);
  const currentWorkingStateFingerprint =
    fingerprintReviewedEnvironmentWorkingStateSync(state.projection);
  if (currentWorkingStateFingerprint !== input.review.workingStateFingerprint) {
    return yield* new Conflict({
      message: "Working State changed after the manual deployment was reviewed.",
    });
  }

  const saved = yield* publishEnvironmentSavedState({
    environmentId: input.environmentId,
    actorId: input.actorId,
    message: input.message,
    basis: input.review.savedStateBasis,
    intent: state.intent,
    destructiveVolumeReviews: [],
    revisionPolicy: "reuse_latest_if_equivalent",
  });

  const deployment = yield* admitEnvironmentDeployment({
    environmentId: input.environmentId,
    triggerOrigin: { origin: "manual", actorId: input.actorId },
    message: input.message,
    savedStateSnapshotId: saved.savedStateSnapshotId,
    serviceActionPolicy: { kind: "all_affected_required" },
  });

  return {
    environmentDeploymentId: deployment.id,
    status: deployment.status,
    createdAt: deployment.createdAt,
    serviceCount: saved.nodeSnapshots.filter(
      ({ nodeType }) => nodeType === "service",
    ).length,
  };
});
