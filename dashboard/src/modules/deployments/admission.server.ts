import { deploymentSourcePinsSchema, validateDeploymentSourcePins, type DeploymentSourcePins } from "./source-pins";
import { serviceRegistryCredential, service as serviceIdentity } from "#/modules/environment-design/tables";
import { parseServiceConfig } from "@ployz/sdk/config";
import "@tanstack/react-start/server-only";

import { randomUUID } from "node:crypto";
import {
  and,
  desc,
  eq,
  inArray,
  not,
  type SQL,
} from "drizzle-orm";
import { Effect, Schema } from "effect";
import {
  environmentDeployment as schemaEnvironmentDeployment,
  environmentDeploymentSecret,
} from "#/modules/deployments/tables";
import { volumeIsAuthored } from "#/modules/environment-design/document-identity.server";
import {
  environmentResource as schemaEnvironmentResource,
} from "#/modules/environment-design/tables";
import {
  environmentNodeConfigSnapshot as schemaEnvironmentNodeConfigSnapshot,
  environmentNodeConfigSnapshotSecret as schemaEnvironmentNodeConfigSnapshotSecret,
  volumeRemoveAttempt as schemaVolumeRemoveAttempt,
} from "#/modules/runtime/tables";
import type { EncryptedSecretValue } from "#/db/tables";
import type { EnvironmentDeploymentServiceActionPolicy } from "#/modules/deployments/tables";
import {
  organizationIdForDeployment,
  organizationIdForEnvironment,
} from "#/db/scope-values.server";
import { projectJsonObject } from "#/lib/json";
import { lockEnvironmentDeploymentQueue } from "#/modules/deployments/queue-lock.server";
import { getVolumePhysicalName } from "#/modules/environment-design/volume-config";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { stageVolumeRemoveAttempt } from "#/modules/runtime/volume-removal.repository";
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
  credentialRevision?: string | null;
  encryptedRegistryUsername?: EncryptedSecretValue | null;
  encryptedRegistrySecret?: EncryptedSecretValue | null;
};

export type SavedDeploymentTarget = CompiledSavedEnvironmentIntent & {
  readonly savedStateSnapshotId: string;
  readonly requestedByUserId: string;
  readonly volumeDeletionAuthorizations: readonly DestructiveVolumeReview[];
};

export type DeploymentAdmissionInput = {
  readonly environmentId: string;
  readonly savedStateSnapshotId: string;
  readonly triggerOrigin: DeploymentTriggerOriginType;
  readonly message: string | null;
  readonly serviceActionPolicy?: EnvironmentDeploymentServiceActionPolicy | null;
  readonly retryOfDeploymentId?: string | null;
  readonly sourcePins?: DeploymentSourcePins;
  readonly freshVolumeReviewIds?: readonly string[];
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
      requestedByUserId: saved.actorId,
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
    requestedByUserId: saved.actorId,
    ...compileSavedEnvironmentIntent({ environmentId, intent: saved.intent }),
    volumeDeletionAuthorizations: saved.volumeDeletionAuthorizations,
  } satisfies SavedDeploymentTarget;
});

function insertNodeSnapshots(input: {
  environmentId: string;
  environmentDeploymentId: string;
  nodeSnapshots: readonly PreparedEnvironmentNodeSnapshot[];
  retryOfDeploymentId?: string | null;
}) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    if (input.nodeSnapshots.length === 0) return;
    const credentials = yield* drizzle.select({ credential: serviceRegistryCredential })
      .from(serviceRegistryCredential).innerJoin(serviceIdentity, eq(serviceIdentity.id, serviceRegistryCredential.serviceId))
      .where(eq(serviceIdentity.environmentId, input.environmentId));
    const frozen = input.retryOfDeploymentId
      ? yield* drizzle.select({ nodeId: schemaEnvironmentNodeConfigSnapshot.nodeId, secret: schemaEnvironmentNodeConfigSnapshotSecret })
          .from(schemaEnvironmentNodeConfigSnapshot)
          .innerJoin(schemaEnvironmentNodeConfigSnapshotSecret, eq(schemaEnvironmentNodeConfigSnapshotSecret.snapshotId, schemaEnvironmentNodeConfigSnapshot.id))
          .where(and(eq(schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId, input.retryOfDeploymentId),
            eq(schemaEnvironmentNodeConfigSnapshot.environmentId, input.environmentId)))
      : [];
    const prepared = [];
    for (const snapshot of input.nodeSnapshots) {
      if (snapshot.nodeType !== "service" || snapshot.encryptedRegistrySecret) { prepared.push(snapshot); continue; }
      const source = parseServiceConfig(snapshot.config).source;
      if (source.type !== "image" || source.credentials.type === "none") { prepared.push(snapshot); continue; }
      const credentialId = source.credentials.credentialId;
      const previous = frozen.find(row => row.nodeId === snapshot.nodeId)?.secret;
      const credential = input.retryOfDeploymentId
        ? previous && { ...previous, revision: previous.credentialRevision }
        : credentials.find(row => row.credential.serviceId === credentialId)?.credential;
      if (!credential?.encryptedRegistrySecret) return yield* new Conflict({ message: "Registry credentials are unavailable." });
      prepared.push({ ...snapshot, credentialRevision: credential.revision,
        encryptedRegistryUsername: credential.encryptedRegistryUsername,
        encryptedRegistrySecret: credential.encryptedRegistrySecret });
    }
    const snapshots = prepared.map((snapshot) => {
      const {
        encryptedRegistryUsername,
        encryptedRegistrySecret,
        credentialRevision,
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
        credentialRevision: credentialRevision ?? null,
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
              organizationId: snapshot.configSnapshot.organizationId,
              snapshotId: snapshot.configSnapshot.id,
              credentialRevision: snapshot.credentialRevision,
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
            not(volumeIsAuthored),
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

function stageReviewedVolumeRemoveAttempt(input: {
  organizationId: string | SQL;
  requestedByUserId: string;
  environmentId: string;
  environmentDeploymentId: string;
  environmentResourceId: string;
  target: DestructiveVolumeReview["target"];
  evidence: DestructiveVolumeReview["evidence"];
}) {
  return Effect.gen(function* () {
    const target = input.target;
    const testimony = input.evidence.evidence;
    if (
      target.resourceId !== input.environmentResourceId ||
      target.volumeName !== getVolumePhysicalName(input.environmentResourceId) ||
      input.evidence.fingerprint.length === 0 ||
      target.namespaceId !== testimony.namespaceId ||
      target.volumeName !== testimony.volumeName ||
      target.machineId !== testimony.machineId
    ) {
      return yield* new Validation({
        message:
          "Saved destructive volume review does not identify the exact tombstoned volume.",
      });
    }
    const machineId = yield* Schema.decodeUnknownEffect(rustMachineIdSchema)(
      target.machineId,
    ).pipe(
      Effect.mapError(
        () =>
          new Validation({
            message:
              "Saved destructive volume review has an invalid machine ID.",
          }),
      ),
    );
    return yield* stageVolumeRemoveAttempt({
      organizationId: input.organizationId,
      requestedByUserId: input.requestedByUserId,
      environmentId: input.environmentId,
      environmentDeploymentId: input.environmentDeploymentId,
      environmentResourceId: input.environmentResourceId,
      volumes: [
        {
          machine_id: machineId,
          name: target.volumeName,
        },
      ],
    });
  });
}

function writeQueuedSavedTarget(
  input: DeploymentAdmissionInput,
  target: SavedDeploymentTarget,
) {
  return Effect.gen(function* () {
    const { drizzle } = yield* Database;
    let sourcePins = input.sourcePins ?? {};
    if (input.retryOfDeploymentId) {
      const [previous] = yield* drizzle.select({ sourcePins: schemaEnvironmentDeployment.sourcePins })
        .from(schemaEnvironmentDeployment).where(and(eq(schemaEnvironmentDeployment.id, input.retryOfDeploymentId),
          eq(schemaEnvironmentDeployment.environmentId, input.environmentId),
          eq(schemaEnvironmentDeployment.savedStateSnapshotId, input.savedStateSnapshotId)));
      if (!previous) return yield* new Conflict({ message: "Retry source attempt does not match the Saved revision." });
      sourcePins = previous.sourcePins;
    }
    sourcePins = yield* Schema.decodeUnknownEffect(deploymentSourcePinsSchema)(sourcePins, strictParseOptions)
      .pipe(Effect.mapError(() => new Conflict({ message: "Deployment source pins are invalid." })));
    sourcePins = yield* validateDeploymentSourcePins(sourcePins, target.nodeSnapshots);
    const queuedRows = yield* drizzle
      .select({
        id: schemaEnvironmentDeployment.id,
        createdAt: schemaEnvironmentDeployment.createdAt,
        triggerOrigin: schemaEnvironmentDeployment.triggerOrigin,
        inngestRunId: schemaEnvironmentDeployment.inngestRunId,
      })
      .from(schemaEnvironmentDeployment)
      .where(
        and(
          eq(schemaEnvironmentDeployment.environmentId, input.environmentId),
          eq(schemaEnvironmentDeployment.status, "queued"),
        ),
      )
      .for("update")
      .limit(1);
    const queued = queuedRows[0];
    if (queued && (input.triggerOrigin.origin === "manual" || queued.triggerOrigin.origin === "manual" || queued.inngestRunId !== null)) {
      return yield* new Conflict({
        message: "An environment deployment is already queued. Wait for it to start or cancel it before deploying again.",
      });
    }
    const now = new Date();
    const deploymentRows = queued
      ? yield* drizzle
          .update(schemaEnvironmentDeployment)
          .set({
            triggerOrigin: input.triggerOrigin,
            message: input.message,
            retryOfDeploymentId: input.retryOfDeploymentId ?? null,
            sourcePins,
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
            sourcePins,
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
    yield* drizzle
      .insert(environmentDeploymentSecret)
      .values({ organizationId: organizationIdForDeployment(deployment.id), environmentDeploymentId: deployment.id })
      .onConflictDoNothing();
    if (queued !== undefined) {
      yield* drizzle
        .delete(schemaVolumeRemoveAttempt)
        .where(
          eq(
            schemaVolumeRemoveAttempt.environmentDeploymentId,
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
      retryOfDeploymentId: input.retryOfDeploymentId,
    });
    const authorizations = yield* actionableVolumeDeletionAuthorizations(
      input.environmentId,
      target.volumeDeletionAuthorizations,
    );
    if (authorizations.length) {
      const previousRemovals = yield* drizzle.select({ resourceId: schemaVolumeRemoveAttempt.environmentResourceId })
        .from(schemaVolumeRemoveAttempt).where(and(
          eq(schemaVolumeRemoveAttempt.environmentId, input.environmentId),
          inArray(schemaVolumeRemoveAttempt.environmentResourceId, authorizations.map(review => review.target.resourceId)),
        ));
      const freshlyReviewed = new Set(input.freshVolumeReviewIds);
      if (previousRemovals.some(attempt => !freshlyReviewed.has(attempt.resourceId ?? ""))) {
        return yield* new Conflict({ message: "Volume removal retries require a fresh destructive review from the environment canvas." });
      }
    }
    yield* Effect.forEach(
      authorizations,
      (authorization) =>
        stageReviewedVolumeRemoveAttempt({
          organizationId: organizationIdForEnvironment(input.environmentId),
          requestedByUserId: target.requestedByUserId,
          environmentId: input.environmentId,
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
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockEnvironmentDeploymentQueue(input.environmentId);
    const target = yield* loadExactSavedDeploymentTarget(input);
    return yield* writeQueuedSavedTarget({ ...input, triggerOrigin }, target);
  }));
});
