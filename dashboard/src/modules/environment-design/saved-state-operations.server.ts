import "@tanstack/react-start/server-only";

import { reusePublication } from "@ployz/sdk/config";
import { Effect, Schema } from "effect";
import {
  environmentSavedStateSnapshot as schemaEnvironmentSavedStateSnapshot,
} from "#/modules/deployments/tables";
import { organizationIdForEnvironment } from "#/db/scope-values.server";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import {
  loadCurrentEnvironmentState,
  type CurrentEnvironmentSnapshotProjection,
} from "#/modules/environment-design/working-state-repository.server";
import {
  loadEnvironmentSnapshotProjection,
} from "#/modules/deployments/environment-state.repository.server";
import {
  canonicalizeSavedEnvironmentIntent,
  compileSavedEnvironmentIntent,
  encodePersistedSavedEnvironmentIntent,
  savedEnvironmentIntentSchema,
  type CompiledSavedEnvironmentIntent,
  type SavedEnvironmentIntent,
} from "./saved-intent";
import { strictParseOptions } from "./schema";
import {
  destructiveVolumeReviewsSchema,
  resolveSavedVolumeDeletionAuthorizations,
  type DestructiveVolumeReview,
} from "./destructive-volume-review";
import {
  loadLatestEnvironmentSavedState,
} from "./saved-state-repository.server";
import {
  environmentSavedStateBasisMatches,
  type EnvironmentSavedStateBasis,
} from "./saved-state";
import { fingerprintReviewedEnvironmentWorkingStateSync } from "./working-state-fingerprint.server";
import {
  getDestructiveEnvironmentSaveReviewMismatch,
  projectDestructiveEnvironmentSave,
  projectReviewedEnvironmentPublicationDestructiveSave,
  type ReviewedEnvironmentPublication,
} from "./working-state-review";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";

export type EnvironmentSavedStatePublication =
  CompiledSavedEnvironmentIntent & {
    savedStateSnapshotId: string;
    intent: SavedEnvironmentIntent;
    volumeDeletionAuthorizations: DestructiveVolumeReview[];
    revisionCreated: boolean;
  };

/** The sole immutable Saved revision writer. */
export const publishEnvironmentSavedState = Effect.fn(
  "EnvironmentDesign.publishEnvironmentSavedState",
)(function* (input: {
  environmentId: string;
  actorId: string;
  message: string | null;
  basis: EnvironmentSavedStateBasis;
  intent: SavedEnvironmentIntent;
  destructiveVolumeReviews: readonly DestructiveVolumeReview[];
  revisionPolicy: "always_create" | "reuse_latest_if_equivalent";
}) {
  const { drizzle } = yield* Database;
  yield* lockEnvironmentDeploymentQueue(input.environmentId);
  const decoded = yield* Schema.decodeUnknownEffect(savedEnvironmentIntentSchema)(
    input.intent,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      () =>
        new Conflict({
          message: "Environment Saved State is invalid.",
        }),
    ),
  );
  const canonical = yield* Effect.try({
    try: () => {
      const intent = canonicalizeSavedEnvironmentIntent(decoded);
      return {
        intent,
        ...compileSavedEnvironmentIntent({
          environmentId: input.environmentId,
          intent,
        }),
      };
    },
    catch: () =>
      new Conflict({
        message: "Environment Saved State is invalid.",
      }),
  });
  const latest = yield* loadLatestEnvironmentSavedState(input.environmentId);
  if (!environmentSavedStateBasisMatches(input.basis, latest?.id ?? null)) {
    return yield* new Conflict({
      message:
        "Saved State changed after this action was reviewed. Review the latest changes and try again.",
    });
  }
  const volumeDeletionAuthorizations =
    resolveSavedVolumeDeletionAuthorizations({
      previous: latest?.volumeDeletionAuthorizations ?? [],
      fresh: yield* Schema.decodeUnknownEffect(destructiveVolumeReviewsSchema)(
        input.destructiveVolumeReviews,
        strictParseOptions,
      ).pipe(
        Effect.mapError(
          () =>
            new Conflict({
              message: "Saved volume deletion authority is invalid.",
            }),
        ),
      ),
      presentVolumeIds: new Set(
        canonical.intent.volumes.map((volume) => volume.resourceId),
      ),
    });

  if (latest && reusePublication({
    policy: input.revisionPolicy,
    current: { intent: canonical.intent, volumeDeletionAuthorizations },
    latest: { intent: latest.intent, volumeDeletionAuthorizations: latest.volumeDeletionAuthorizations },
  })) {
    return {
      ...canonical,
      savedStateSnapshotId: latest.id,
      volumeDeletionAuthorizations,
      revisionCreated: false,
    } satisfies EnvironmentSavedStatePublication;
  }

  const rows = yield* drizzle
    .insert(schemaEnvironmentSavedStateSnapshot)
    .values({
      organizationId: organizationIdForEnvironment(input.environmentId),
      environmentId: input.environmentId,
      actorId: input.actorId,
      message: input.message,
      ...encodePersistedSavedEnvironmentIntent({ intent: canonical.intent }),
      volumeDeletionAuthorizations,
    })
    .returning({ id: schemaEnvironmentSavedStateSnapshot.id });
  const inserted = rows[0];
  if (inserted === undefined) {
    return yield* Effect.die(
      new Error("Saved State insert returned no revision."),
    );
  }
  return {
    ...canonical,
    savedStateSnapshotId: inserted.id,
    volumeDeletionAuthorizations,
    revisionCreated: true,
  } satisfies EnvironmentSavedStatePublication;
});

const validateEnvironmentPublicationReview = Effect.fn(
  "EnvironmentDesign.validateEnvironmentPublicationReview",
)(function* (input: {
  environmentId: string;
  workingNodes: CurrentEnvironmentSnapshotProjection["nodeSnapshots"];
  review: ReviewedEnvironmentPublication;
}) {
  const projection = yield* loadEnvironmentSnapshotProjection({
    kind: "environment",
    environmentId: input.environmentId,
  });
  const explicitState = projection.explicitStates.find(
    (state) => state.environmentId === input.environmentId,
  );
  const mismatch = getDestructiveEnvironmentSaveReviewMismatch({
    expected: projectDestructiveEnvironmentSave({
      workingNodes: input.workingNodes,
      savedNodes: explicitState?.saved?.nodes ?? [],
      appliedNodes: explicitState?.applied.nodes ?? [],
    }),
    reviewed: projectReviewedEnvironmentPublicationDestructiveSave(
      input.review,
    ),
  });
  if (mismatch !== null) {
    return yield* new Conflict({ message: mismatch });
  }
});

/** Publishes the exact reviewed Working graph without admitting a deployment. */
export const saveReviewedEnvironmentState = Effect.fn(
  "EnvironmentDesign.saveReviewedEnvironmentState",
)(function* (input: {
  environmentId: string;
  actorId: string;
  message: string | null;
  review: ReviewedEnvironmentPublication;
}) {
  yield* lockEnvironmentDeploymentQueue(input.environmentId);
  const state = yield* loadCurrentEnvironmentState(input.environmentId);
  const currentWorkingStateFingerprint =
    fingerprintReviewedEnvironmentWorkingStateSync(state.projection);
  if (
    currentWorkingStateFingerprint !== input.review.workingStateFingerprint
  ) {
    return yield* new Conflict({
      message: "Working State changed after the Save was reviewed.",
    });
  }
  yield* validateEnvironmentPublicationReview({
    environmentId: input.environmentId,
    workingNodes: state.projection.nodeSnapshots,
    review: input.review,
  });
  const saved = yield* publishEnvironmentSavedState({
    environmentId: input.environmentId,
    actorId: input.actorId,
    message: input.message,
    basis: input.review.savedStateBasis,
    intent: state.intent,
    destructiveVolumeReviews: input.review.destructiveVolumeReviews,
    revisionPolicy: "always_create",
  });
  return { savedStateSnapshotId: saved.savedStateSnapshotId };
});
