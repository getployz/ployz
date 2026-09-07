import "@tanstack/react-start/server-only";

import { isDeepStrictEqual } from "node:util";
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
  discardSavedServiceIntentSetting,
  replaceSavedEnvironmentIntentNode,
} from "./saved-intent-mutations";
import {
  destructiveVolumeReviewsSchema,
  resolveSavedVolumeDeletionAuthorizations,
  type DestructiveVolumeReview,
} from "./destructive-volume-review";
import {
  loadAppliedServiceSavedIntents,
  loadEnvironmentSavedIntentById,
  loadLatestEnvironmentSavedState,
} from "./saved-state-repository.server";
import {
  environmentSavedStateBasisMatches,
  type EnvironmentSavedStateBasis,
  type EnvironmentSavedStateDiscardCommand,
} from "./saved-state";
import { fingerprintReviewedEnvironmentWorkingStateSync } from "./working-state-fingerprint.server";
import {
  getDestructiveEnvironmentSaveReviewMismatch,
  projectDestructiveEnvironmentSave,
  projectReviewedEnvironmentPublicationDestructiveSave,
  type ReviewedEnvironmentPublication,
} from "./working-state-review";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";

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

  if (
    input.revisionPolicy === "reuse_latest_if_equivalent" &&
    latest &&
    isDeepStrictEqual(
      canonicalizeSavedEnvironmentIntent(latest.intent),
      canonical.intent,
    ) &&
    isDeepStrictEqual(
      latest.volumeDeletionAuthorizations,
      volumeDeletionAuthorizations,
    )
  ) {
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

/** Applies all basis-bound reset operations before publishing one revision. */
export const discardEnvironmentSavedState = Effect.fn(
  "EnvironmentDesign.discardEnvironmentSavedState",
)(function* (input: {
  environmentId: string;
  actorId: string;
  command: EnvironmentSavedStateDiscardCommand;
}) {
  yield* lockEnvironmentDeploymentQueue(input.environmentId);
  const projection = yield* loadEnvironmentSnapshotProjection({
    kind: "environment",
    environmentId: input.environmentId,
  });
  const latest = yield* loadLatestEnvironmentSavedState(input.environmentId);
  if (latest === null) {
    return yield* new NotFound({
      message: "Saved Environment State not found.",
    });
  }
  if (!environmentSavedStateBasisMatches(input.command.basis, latest.id)) {
    return yield* new Conflict({
      message:
        "Saved State changed after this discard was planned. Review the latest changes and try again.",
    });
  }

  let intent = latest.intent;
  for (const change of input.command.operations) {
    const key = `${change.nodeType}:${change.nodeId}`;
    const appliedNode = projection.appliedSavedNodeByKey.get(key) ?? null;
    let baselineIntent: SavedEnvironmentIntent | null = null;
    if (appliedNode !== null) {
      const baseline = yield* loadEnvironmentSavedIntentById({
        environmentId: input.environmentId,
        savedStateSnapshotId: appliedNode.sourceSavedStateSnapshotId,
      });
      if (baseline === null) {
        return yield* new Conflict({
          message: "The Applied State is missing.",
        });
      }
      baselineIntent = baseline.intent;
    }

    if (change.kind === "node") {
      if (baselineIntent !== null && change.nodeType !== "service") {
        baselineIntent = {
          ...baselineIntent,
          services: yield* loadAppliedServiceSavedIntents({
            environmentId: input.environmentId,
            services: [...projection.appliedSavedNodeByKey.values()].filter(
              (node) => node.nodeType === "service",
            ),
          }),
        };
      }
      intent = yield* replaceSavedEnvironmentIntentNode({
        current: intent,
        baseline: baselineIntent,
        node: change,
      });
      continue;
    }

    if (baselineIntent === null) {
      return yield* new Validation({
        field: "command",
        message: "The pending Service setting no longer exists.",
      });
    }
    intent = yield* discardSavedServiceIntentSetting({
      current: intent,
      baseline: baselineIntent,
      serviceId: change.nodeId,
      path: change.setting,
    });
  }

  const published = yield* publishEnvironmentSavedState({
    environmentId: input.environmentId,
    actorId: input.actorId,
    message: "Discard pending change",
    basis: input.command.basis,
    intent,
    destructiveVolumeReviews: [],
    revisionPolicy: "always_create",
  });
  return { savedStateSnapshotId: published.savedStateSnapshotId };
});
