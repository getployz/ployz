import "@tanstack/react-start/server-only";
import { compareServiceSettings, parseServiceConfig } from "@ployz/sdk/config";
import type { Actor } from "#/modules/identity/actor";
import { withMutationResult } from "#/server/mutation-result.server";
import { requireEnvironmentForActorById } from "./authoring-repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
import { loadEnvironmentSavedIntentById } from "./saved-state-repository.server";
import { emptyEnvironmentIntent, restoreDashboardEnvironmentNode } from "./saved-intent";
import { loadEnvironmentNodeIntroductionIntent } from "./environment-node-introduction.repository.server";
import type { DiscardEnvironmentChangesInput } from "./working-document-restore";

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
  type EnvironmentSnapshotProjection,
} from "#/modules/deployments/environment-state.repository.server";
import {
  canonicalizeSavedEnvironmentIntent,
  compileSavedEnvironmentIntent,
  encodePersistedSavedEnvironmentIntent,
  reuseSavedEnvironmentPublication,
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
const publishEnvironmentSavedState = Effect.fn(
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

  if (latest && reuseSavedEnvironmentPublication({
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
  revisionPolicy?: "always_create" | "reuse_latest_if_equivalent";
}) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockEnvironmentDeploymentQueue(input.environmentId);
    yield* loadEnvironmentDocument(input.environmentId, true);
    const state = yield* loadCurrentEnvironmentState(input.environmentId);
    const currentWorkingStateFingerprint =
      fingerprintReviewedEnvironmentWorkingStateSync(state.projection);
    if (
      currentWorkingStateFingerprint !== input.review.workingStateFingerprint
    ) {
      return yield* new Conflict({
        message: "Working State changed after publication was reviewed.",
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
      revisionPolicy: input.revisionPolicy ?? "always_create",
    });
    return saved;
  }));
});

/** Reconstruct authored Applied State from each node's confirmed Saved revision. */
const loadAppliedIntent = Effect.fn("EnvironmentDesign.loadAppliedIntent")(
  function* (environmentId: string, namespace: string, projection: EnvironmentSnapshotProjection) {
    const nodes = [...projection.appliedSavedNodeByKey.values()];
    const intents = new Map<string, SavedEnvironmentIntent>();
    for (const savedStateSnapshotId of new Set(nodes.map(node => node.sourceSavedStateSnapshotId))) {
      const saved = yield* loadEnvironmentSavedIntentById({ environmentId, savedStateSnapshotId });
      if (!saved) return yield* new Conflict({ message: "Applied State is missing its authored revision." });
      intents.set(savedStateSnapshotId, saved.intent);
    }
    const baseline = emptyEnvironmentIntent(namespace);
    for (const node of nodes) {
      const source = intents.get(node.sourceSavedStateSnapshotId);
      if (!source) return yield* new Conflict({ message: "Applied State is missing its authored revision." });
      if (node.nodeType === "service") {
        const value = source.services.find(value => value.id === node.nodeId);
        if (!value) return yield* new Conflict({ message: "Applied Service is missing." });
        baseline.services.push(value);
      } else {
        const value = source.volumes.find(value => value.resourceId === node.nodeId);
        if (!value) return yield* new Conflict({ message: "Applied Volume is missing." });
        baseline.volumes.push(value);
      }
    }
    return baseline;
  },
);

/** One transaction restores the reviewed scope in Working and Saved State. */
export const discardEnvironmentChanges = Effect.fn("EnvironmentDesign.discardEnvironmentChanges")(
  function* (actor: Actor, input: DiscardEnvironmentChangesInput) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationResult(Effect.gen(function* () {
      yield* lockEnvironmentDeploymentQueue(input.environmentId);
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      const projection = yield* loadEnvironmentSnapshotProjection({ kind: "environment", environmentId: input.environmentId });
      const state = projection.explicitStates.find(state => state.environmentId === input.environmentId);
      const submitted = state?.deploymentEvidence;
      const head = submitted ?? state?.applied;
      const headToken = head?.token ?? "applied:none";
      const latest = yield* loadLatestEnvironmentSavedState(input.environmentId);
      if (headToken !== input.headToken || !environmentSavedStateBasisMatches(input.savedStateBasis, latest?.id ?? null)) {
        return yield* new Conflict({ message: "Environment changes moved after this review. Review the latest changes and try again." });
      }
      let baseline: SavedEnvironmentIntent;
      if (submitted) {
        const saved = yield* loadEnvironmentSavedIntentById({
          environmentId: input.environmentId, savedStateSnapshotId: submitted.savedStateSnapshotId,
        });
        if (!saved) return yield* new Conflict({ message: "Submitted Saved State is missing." });
        baseline = saved.intent;
      } else {
        baseline = yield* loadAppliedIntent(input.environmentId, document.namespace, projection);
      }
      const command = input.command;
      const nodeAt = (nodes: Array<{ nodeType: string; nodeId: string; config: unknown }> = []) => command.kind === "node"
        ? nodes.find(node => node.nodeType === command.nodeType && node.nodeId === command.nodeId)?.config ?? null : null;
      const savedNode = nodeAt(state?.saved?.nodes);
      const headNode = nodeAt(head?.nodes);
      const introduction = command.kind === "node" && command.path !== undefined && headNode === null;
      if (introduction && (savedNode !== null || nodeAt(state?.applied.nodes) !== null)) {
        return yield* new Conflict({ message: "This field has no discard baseline." });
      }
      if (introduction) {
        baseline = yield* loadEnvironmentNodeIntroductionIntent({
          environmentId: input.environmentId, nodeType: command.nodeType, nodeId: command.nodeId,
        });
      }
      const restore = (current: SavedEnvironmentIntent) => Effect.try({
        try: () => command.kind === "all" ? baseline
          : restoreDashboardEnvironmentNode(current, baseline, command, command.path),
        catch: () => new Conflict({ message: "Discard would leave invalid Environment relationships." }),
      });
      const working = yield* restore(document.intent);
      // Saved follows so a non-manual trigger ships the discarded state.
      const savedNeedsRestore = command.kind === "all" || !command.path ||
        (savedNode != null && headNode != null &&
          compareServiceSettings(parseServiceConfig(savedNode), parseServiceConfig(headNode))
            .some(row => row.path === command.path && row.canRestore));
      // New-node field resets use its Introduction and do not publish it.
      if (latest && !introduction && savedNeedsRestore) {
        const saved = yield* restore(latest.intent);
        yield* publishEnvironmentSavedState({
          environmentId: input.environmentId, actorId: actor.userId,
          message: "Discard changes", basis: input.savedStateBasis, intent: saved,
          destructiveVolumeReviews: [], revisionPolicy: "reuse_latest_if_equivalent",
        });
      }
      return yield* writeEnvironmentDocument(document, working);
    }));
  },
);
