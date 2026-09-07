import "@tanstack/react-start/server-only";

import { randomUUID } from "node:crypto";
import { and, desc, eq, inArray, isNotNull, isNull, ne } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import {
  environmentResource as schemaEnvironmentResource,
} from "#/modules/environment-design/tables";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";
import {
  environmentNodeConfigSnapshot as schemaEnvironmentNodeConfigSnapshot,
  environmentNodeConfigSnapshotSecret as schemaEnvironmentNodeConfigSnapshotSecret,
} from "#/modules/runtime/tables";
import type { EncryptedSecretValue } from "#/db/tables";
import type { EnvironmentDeploymentServiceActionPolicy } from "#/modules/deployments/tables";
import {
  organizationIdForDeployment,
  organizationIdForEnvironment,
} from "#/db/scope-values.server";
import { projectJsonObject } from "#/lib/json";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import {
  assertDestructiveVolumeReplayIdentity,
  assertReviewedDestructiveVolumeIdentity,
} from "#/modules/operations/destructive-volume-attempt-persistence.server";
import {
  validateDestructiveVolumeStagingAuthority,
} from "#/modules/operations/destructive-volume-attempt-admission.server";
import {
  DestructiveVolumeConflict,
  DestructiveVolumeEvidenceInvalid,
} from "#/modules/operations/destructive-volume-errors";
import type {
  DestructiveVolumeReview,
} from "#/modules/environment-design/destructive-volume-review";
import {
  loadEnvironmentSavedIntentById,
  loadLatestEnvironmentSavedState,
} from "#/modules/environment-design/saved-state-repository.server";
import {
  compileSavedEnvironmentIntent,
  type CompiledSavedEnvironmentIntent,
} from "#/modules/environment-design/saved-intent";
import type {
  EnvironmentResourceNodeConfigByType,
} from "#/modules/environment-design/environment-resource-node";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import { strictParseOptions } from "#/modules/environment-design/schema";
import {
  DeploymentTriggerOrigin,
  type DeploymentTriggerOrigin as DeploymentTriggerOriginType,
} from "./deployment";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";

export type PreparedEnvironmentNodeSnapshot = Pick<
  typeof schemaEnvironmentNodeConfigSnapshot.$inferInsert,
  "environmentId" | "nodeType" | "nodeId" | "nodeLineageId"
> & {
  configVersion: number;
  config:
    | ServiceDeploymentConfig
    | EnvironmentResourceNodeConfigByType[keyof EnvironmentResourceNodeConfigByType];
  encryptedRegistryUsername?: EncryptedSecretValue | null;
  encryptedRegistrySecret?: EncryptedSecretValue | null;
};

export type SavedDeploymentTarget = CompiledSavedEnvironmentIntent & {
  readonly savedStateSnapshotId: string;
  readonly volumeDeletionAuthorizations: readonly DestructiveVolumeReview[];
};

export type DeploymentAdmissionInput = {
  readonly environmentId: string;
  readonly savedStateSnapshotId: string;
  readonly triggerOrigin: DeploymentTriggerOriginType;
  readonly message: string | null;
  readonly serviceActionPolicy?: EnvironmentDeploymentServiceActionPolicy | null;
  readonly retryOfDeploymentId?: string | null;
};

function loadExactSavedDeploymentTarget(input: {
  environmentId: string;
  savedStateSnapshotId: string;
}) {
  return Effect.gen(function* () {
    const saved = yield* loadEnvironmentSavedIntentById(input);
    if (saved === null) {
      return yield* new NotFound({
        message: "Saved Environment State not found.",
      });
    }
    return {
      savedStateSnapshotId: saved.id,
      ...compileSavedEnvironmentIntent({
        environmentId: input.environmentId,
        intent: saved.intent,
      }),
      volumeDeletionAuthorizations: saved.volumeDeletionAuthorizations,
    } satisfies SavedDeploymentTarget;
  });
}

export const loadLatestSavedDeploymentTarget = Effect.fn(
  "Deployments.loadLatestSavedDeploymentTarget",
)(function* (environmentId: string) {
  yield* lockEnvironmentDeploymentQueue(environmentId);
  const saved = yield* loadLatestEnvironmentSavedState(environmentId);
  if (saved === null) {
    return yield* new Conflict({
      message: "Deployment requires a Saved Environment State.",
    });
  }
  return {
    savedStateSnapshotId: saved.id,
    ...compileSavedEnvironmentIntent({ environmentId, intent: saved.intent }),
    volumeDeletionAuthorizations: saved.volumeDeletionAuthorizations,
  } satisfies SavedDeploymentTarget;
});

function insertNodeSnapshots(input: {
  environmentId: string;
  environmentDeploymentId: string;
  nodeSnapshots: readonly PreparedEnvironmentNodeSnapshot[];
}) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    if (input.nodeSnapshots.length === 0) return;
    const snapshots = input.nodeSnapshots.map((snapshot) => {
      const {
        encryptedRegistryUsername,
        encryptedRegistrySecret,
        ...configSnapshot
      } = snapshot;
      const config = projectJsonObject(configSnapshot.config);
      if (config === null) {
        throw new TypeError("Environment node config must be a JSON object.");
      }
      return {
        configSnapshot: {
          ...configSnapshot,
          config,
          id: randomUUID(),
          organizationId: organizationIdForEnvironment(input.environmentId),
          environmentDeploymentId: input.environmentDeploymentId,
        },
        encryptedRegistryUsername: encryptedRegistryUsername ?? null,
        encryptedRegistrySecret: encryptedRegistrySecret ?? null,
      };
    });
    yield* drizzle
      .insert(schemaEnvironmentNodeConfigSnapshot)
      .values(snapshots.map(({ configSnapshot }) => configSnapshot));
    const secrets = snapshots.flatMap((snapshot) =>
      snapshot.encryptedRegistryUsername || snapshot.encryptedRegistrySecret
        ? [
            {
              snapshotId: snapshot.configSnapshot.id,
              encryptedRegistryUsername: snapshot.encryptedRegistryUsername,
              encryptedRegistrySecret: snapshot.encryptedRegistrySecret,
            },
          ]
        : [],
    );
    if (secrets.length > 0) {
      yield* drizzle
        .insert(schemaEnvironmentNodeConfigSnapshotSecret)
        .values(secrets);
    }
  });
}

function actionableVolumeDeletionAuthorizations(
  environmentId: string,
  authorizations: readonly DestructiveVolumeReview[],
) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    if (authorizations.length === 0) return [];
    const resourceIds = authorizations.map(
      (authorization) => authorization.target.resourceId,
    );
    const [tombstones, appliedRows] = yield* Effect.all([
      drizzle
        .select({ id: schemaEnvironmentResource.id })
        .from(schemaEnvironmentResource)
        .where(
          and(
            eq(schemaEnvironmentResource.environmentId, environmentId),
            eq(schemaEnvironmentResource.implementationType, "volume"),
            isNotNull(schemaEnvironmentResource.deletedAt),
            inArray(schemaEnvironmentResource.id, resourceIds),
          ),
        ),
      drizzle
        .select({ id: schemaEnvironmentDeployment.id })
        .from(schemaEnvironmentDeployment)
        .where(
          and(
            eq(schemaEnvironmentDeployment.environmentId, environmentId),
            eq(schemaEnvironmentDeployment.status, "applied"),
          ),
        )
        .orderBy(desc(schemaEnvironmentDeployment.createdAt))
        .limit(1),
    ]);
    const applied = appliedRows[0];
    if (!applied) return [];
    const appliedVolumes = yield* drizzle
      .select({ nodeId: schemaEnvironmentNodeConfigSnapshot.nodeId })
      .from(schemaEnvironmentNodeConfigSnapshot)
      .where(
        and(
          eq(
            schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
            applied.id,
          ),
          eq(
            schemaEnvironmentNodeConfigSnapshot.environmentId,
            environmentId,
          ),
          eq(schemaEnvironmentNodeConfigSnapshot.nodeType, "volume"),
          inArray(schemaEnvironmentNodeConfigSnapshot.nodeId, resourceIds),
        ),
      );
    const tombstoned = new Set(tombstones.map(({ id }) => id));
    const appliedVolumeIds = new Set(
      appliedVolumes.map(({ nodeId }) => nodeId),
    );
    return authorizations.filter(
      (authorization) =>
        tombstoned.has(authorization.target.resourceId) &&
        appliedVolumeIds.has(authorization.target.resourceId),
    );
  });
}

function mapDestructiveVolumeAssertError(
  error: DestructiveVolumeEvidenceInvalid | DestructiveVolumeConflict,
) {
  switch (error._tag) {
    case "DestructiveVolumeEvidenceInvalid":
      return new Validation({ message: error.message });
    case "DestructiveVolumeConflict":
      return new Conflict({ message: error.message });
    default: {
      const _exhaustive: never = error;
      return _exhaustive;
    }
  }
}

function stageDestructiveVolumeAttempt(input: {
  environmentDeploymentId: string;
  environmentResourceId: string;
  target: DestructiveVolumeReview["target"];
  evidence: DestructiveVolumeReview["evidence"];
}) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* assertReviewedDestructiveVolumeIdentity(input).pipe(
      Effect.mapError(mapDestructiveVolumeAssertError),
    );
    const [removalDeployment] = yield* drizzle
      .select({
        environmentId: schemaEnvironmentDeployment.environmentId,
        status: schemaEnvironmentDeployment.status,
      })
      .from(schemaEnvironmentDeployment)
      .where(
        eq(
          schemaEnvironmentDeployment.id,
          input.environmentDeploymentId,
        ),
      )
      .limit(1);
    if (!removalDeployment) {
      return yield* new Conflict({
        message: "Destructive volume staging deployment was not found.",
      });
    }
    const tombstones = yield* drizzle
      .select({ id: schemaEnvironmentResource.id })
      .from(schemaEnvironmentResource)
      .where(
        and(
          eq(schemaEnvironmentResource.id, input.environmentResourceId),
          eq(
            schemaEnvironmentResource.environmentId,
            removalDeployment.environmentId,
          ),
          eq(schemaEnvironmentResource.implementationType, "volume"),
          isNotNull(schemaEnvironmentResource.deletedAt),
        ),
      );
    const [priorApplied] = yield* drizzle
      .select({ id: schemaEnvironmentDeployment.id })
      .from(schemaEnvironmentDeployment)
      .where(
        and(
          eq(
            schemaEnvironmentDeployment.environmentId,
            removalDeployment.environmentId,
          ),
          eq(schemaEnvironmentDeployment.status, "applied"),
          ne(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
        ),
      )
      .orderBy(desc(schemaEnvironmentDeployment.createdAt))
      .limit(1);
    const priorSnapshots = priorApplied
      ? yield* drizzle
          .select({ id: schemaEnvironmentNodeConfigSnapshot.id })
          .from(schemaEnvironmentNodeConfigSnapshot)
          .where(
            and(
              eq(
                schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
                priorApplied.id,
              ),
              eq(
                schemaEnvironmentNodeConfigSnapshot.environmentId,
                removalDeployment.environmentId,
              ),
              eq(schemaEnvironmentNodeConfigSnapshot.nodeType, "volume"),
              eq(
                schemaEnvironmentNodeConfigSnapshot.nodeId,
                input.environmentResourceId,
              ),
            ),
          )
      : [];
    yield* validateDestructiveVolumeStagingAuthority({
      removalDeploymentStatus: removalDeployment.status,
      tombstoneCount: tombstones.length,
      priorAppliedSnapshotCount: priorSnapshots.length,
    }).pipe(Effect.mapError(mapDestructiveVolumeAssertError));
    const loadActive = () =>
      drizzle
        .select()
        .from(schemaDestructiveVolumeAttempt)
        .where(
          and(
            eq(
              schemaDestructiveVolumeAttempt.environmentResourceId,
              input.environmentResourceId,
            ),
            inArray(schemaDestructiveVolumeAttempt.disposition, [
              "active",
              "accepted",
            ]),
          ),
        )
        .limit(1)
        .pipe(Effect.map((rows) => rows[0] ?? null));
    const active = yield* loadActive();
    if (active) {
      yield* assertDestructiveVolumeReplayIdentity(active, input).pipe(
        Effect.mapError(mapDestructiveVolumeAssertError),
      );
      return active;
    }
    const [inserted] = yield* drizzle
      .insert(schemaDestructiveVolumeAttempt)
      .values({
        organizationId: organizationIdForDeployment(
          input.environmentDeploymentId,
        ),
        ...input,
        evidenceFingerprint: input.evidence.fingerprint,
      })
      .onConflictDoNothing()
      .returning();
    if (inserted) return inserted;
    const raced = yield* loadActive();
    if (raced) {
      yield* assertDestructiveVolumeReplayIdentity(raced, input).pipe(
        Effect.mapError(mapDestructiveVolumeAssertError),
      );
      return raced;
    }
    return yield* new Conflict({
      message: "Destructive volume staging lost its active-row race.",
    });
  });
}

function writeQueuedSavedTarget(
  input: DeploymentAdmissionInput,
  target: SavedDeploymentTarget,
) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const queuedRows = yield* drizzle
      .select({
        id: schemaEnvironmentDeployment.id,
        createdAt: schemaEnvironmentDeployment.createdAt,
      })
      .from(schemaEnvironmentDeployment)
      .where(
        and(
          eq(schemaEnvironmentDeployment.environmentId, input.environmentId),
          eq(schemaEnvironmentDeployment.status, "queued"),
          isNull(schemaEnvironmentDeployment.inngestRunId),
        ),
      )
      .for("update")
      .limit(1);
    const queued = queuedRows[0];
    const now = new Date();
    const deploymentRows = queued
      ? yield* drizzle
          .update(schemaEnvironmentDeployment)
          .set({
            triggerOrigin: input.triggerOrigin,
            message: input.message,
            retryOfDeploymentId: input.retryOfDeploymentId ?? null,
            variableProducers: target.variableProducers,
            savedStateSnapshotId: target.savedStateSnapshotId,
            serviceActionPolicy: input.serviceActionPolicy ?? null,
            updatedAt: now,
          })
          .where(eq(schemaEnvironmentDeployment.id, queued.id))
          .returning({
            id: schemaEnvironmentDeployment.id,
            status: schemaEnvironmentDeployment.status,
            createdAt: schemaEnvironmentDeployment.createdAt,
          })
      : yield* drizzle
          .insert(schemaEnvironmentDeployment)
          .values({
            organizationId: organizationIdForEnvironment(input.environmentId),
            environmentId: input.environmentId,
            triggerOrigin: input.triggerOrigin,
            status: "queued",
            message: input.message,
            retryOfDeploymentId: input.retryOfDeploymentId ?? null,
            variableProducers: target.variableProducers,
            savedStateSnapshotId: target.savedStateSnapshotId,
            serviceActionPolicy: input.serviceActionPolicy ?? null,
          })
          .returning({
            id: schemaEnvironmentDeployment.id,
            status: schemaEnvironmentDeployment.status,
            createdAt: schemaEnvironmentDeployment.createdAt,
          });
    const deployment = deploymentRows[0];
    if (deployment === undefined) {
      return yield* Effect.die("Deployment write returned no row.");
    }
    if (queued !== undefined) {
      yield* drizzle
        .delete(schemaDestructiveVolumeAttempt)
        .where(
          eq(
            schemaDestructiveVolumeAttempt.environmentDeploymentId,
            deployment.id,
          ),
        );
      yield* drizzle
        .delete(schemaEnvironmentNodeConfigSnapshot)
        .where(
          eq(
            schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
            deployment.id,
          ),
        );
    }
    yield* insertNodeSnapshots({
      environmentId: input.environmentId,
      environmentDeploymentId: deployment.id,
      nodeSnapshots: target.nodeSnapshots,
    });
    const authorizations = yield* actionableVolumeDeletionAuthorizations(
      input.environmentId,
      target.volumeDeletionAuthorizations,
    );
    yield* Effect.forEach(
      authorizations,
      (authorization) =>
        stageDestructiveVolumeAttempt({
          environmentDeploymentId: deployment.id,
          environmentResourceId: authorization.target.resourceId,
          target: authorization.target,
          evidence: authorization.evidence,
        }),
      { discard: true },
    );
    return {
      ...deployment,
      serviceCount: target.nodeSnapshots.filter(
        ({ nodeType }) => nodeType === "service",
      ).length,
    };
  });
}

/** The sole queued deployment writer. Its immutable target is one exact Saved revision. */
export const admitEnvironmentDeployment = Effect.fn(
  "Deployments.admitEnvironmentDeployment",
)(function* (input: DeploymentAdmissionInput) {
  const triggerOrigin = yield* Schema.decodeUnknownEffect(DeploymentTriggerOrigin)(
    input.triggerOrigin,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      () =>
        new Conflict({
          message: "Deployment trigger identity is invalid.",
        }),
    ),
  );
  yield* lockEnvironmentDeploymentQueue(input.environmentId);
  const target = yield* loadExactSavedDeploymentTarget(input);
  return yield* writeQueuedSavedTarget({ ...input, triggerOrigin }, target);
});
