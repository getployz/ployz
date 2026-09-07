import "@tanstack/react-start/server-only";

import { and, eq, isNull, sql } from "drizzle-orm";
import { Effect } from "effect";
import {
  service as schemaService,
  environmentResource as schemaEnvironmentResource,
  configKey as schemaConfigKey,
  variable as schemaVariable,
  serviceVariableGroupAttachment as schemaServiceVariableGroupAttachment,
  serviceVolumeAttachment as schemaServiceVolumeAttachment,
  variableSecret as schemaVariableSecret,
  configValue as schemaConfigValue,
  serviceRegistryCredential as schemaServiceRegistryCredential,
} from "#/modules/environment-design/tables";
import {
  ServiceWorkingIntentConflict,
  ServiceWorkingIntentNotFound,
  ServiceWorkingIntentPersistenceFailure,
} from "#/modules/services/service-working-intent-errors";
import {
  getDuplicateEnvironmentNodeNameMessage,
  isEnvironmentNodeNameTaken,
  type EnvironmentNodeNameIdentity,
} from "#/modules/environment-design/environment-node-names";
import { decodeStrict } from "#/modules/environment-design/schema";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import {
  loadEnvironmentSavedIntentById,
} from "#/modules/environment-design/saved-state-repository.server";
import { Conflict } from "#/server/public-error";
import {
  restoreServiceWorkingIntentSchema,
  type RestoreServiceWorkingIntentInput,
} from "#/modules/environment-design/services";
import {
  getEnvironmentContextForActorById,
} from "#/modules/environment-design/authoring-repository.server";
import type { Actor } from "#/modules/identity/actor";
import { Database, type DatabaseService } from "#/server/database.server";

type SavedServiceIntent = SavedEnvironmentIntent["services"][number];
type ServiceTransaction = DatabaseService["drizzle"];

export type ServiceWorkingIntentResetCollections = {
  variables: boolean;
  variableGroupAttachments: boolean;
  volumeAttachments: boolean;
};

function savedVariableColumns(
  variable: SavedServiceIntent["variables"][number],
) {
  switch (variable.value.kind) {
    case "literal":
      return {
        valueKind: "plain" as const,
        valueParts: [{ kind: "text" as const, value: variable.value.value }],
        encryptedValue: null,
      };
    case "template":
      return {
        valueKind: "plain" as const,
        valueParts: variable.value.parts,
        encryptedValue: null,
      };
    case "secret":
      return {
        valueKind: "sealed" as const,
        valueParts: null,
        encryptedValue: variable.value.encryptedValue,
      };
  }
}

function listEnvironmentNodeNames(
  environmentId: string,
  transaction: ServiceTransaction,
) {
  return Effect.gen(function* () {
    const [services, resources] = yield* Effect.all([
      transaction
        .select({ id: schemaService.id, name: schemaService.name })
        .from(schemaService)
        .where(
          and(
            eq(schemaService.environmentId, environmentId),
            isNull(schemaService.deletedAt),
          ),
        ),
      transaction
        .select({
          id: schemaEnvironmentResource.id,
          name: schemaEnvironmentResource.name,
          implementationType: schemaEnvironmentResource.implementationType,
        })
        .from(schemaEnvironmentResource)
        .where(eq(schemaEnvironmentResource.environmentId, environmentId)),
    ]);
    return [
      ...services.map((service) => ({
        type: "service" as const,
        id: service.id,
        name: service.name,
      })),
      ...resources.flatMap((resource) =>
        resource.implementationType === "variable_group"
          ? [
              {
                type: "variable_group" as const,
                id: resource.id,
                name: resource.name,
              },
            ]
          : [],
      ),
    ] satisfies EnvironmentNodeNameIdentity[];
  });
}

function ensureConfigKey(
  transaction: ServiceTransaction,
  input: { projectId: string; lineageId: string; key: string },
) {
  return Effect.gen(function* () {
    const [created] = yield* transaction
      .insert(schemaConfigKey)
      .values({
        projectId: input.projectId,
        scope: "service_lineage",
        serviceLineageId: input.lineageId,
        canonicalName: input.key,
      })
      .onConflictDoNothing()
      .returning({ id: schemaConfigKey.id });
    if (created) return created.id;
    const [existing] = yield* transaction
      .select({ id: schemaConfigKey.id })
      .from(schemaConfigKey)
      .where(
        and(
          eq(schemaConfigKey.projectId, input.projectId),
          eq(schemaConfigKey.scope, "service_lineage"),
          eq(schemaConfigKey.serviceLineageId, input.lineageId),
          eq(schemaConfigKey.canonicalName, input.key),
        ),
      )
      .limit(1);
    if (existing) return existing.id;
    return yield* Effect.fail(
      new Error(`Failed to create config key ${input.key}.`),
    );
  });
}

function replaceServiceWorkingIntent(input: {
  organizationId: string;
  projectId: string;
  environmentId: string;
  currentLineageId: string;
  saved: SavedServiceIntent;
  transaction: ServiceTransaction;
}) {
  return Effect.gen(function* () {
    const { saved, transaction } = input;
    const [
      currentVariables,
      currentVariableGroupAttachments,
      currentVolumeAttachments,
    ] = yield* Effect.all([
      transaction
        .select({ id: schemaVariable.id })
        .from(schemaVariable)
        .where(eq(schemaVariable.serviceId, saved.id)),
      transaction
        .select({
          variableGroupId: schemaServiceVariableGroupAttachment.variableGroupId,
        })
        .from(schemaServiceVariableGroupAttachment)
        .where(eq(schemaServiceVariableGroupAttachment.serviceId, saved.id)),
      transaction
        .select({
          volumeResourceId: schemaServiceVolumeAttachment.volumeResourceId,
        })
        .from(schemaServiceVolumeAttachment)
        .where(eq(schemaServiceVolumeAttachment.serviceId, saved.id)),
    ]);

    const hasRegistryCredential =
      saved.encryptedRegistryUsername !== null ||
      saved.encryptedRegistrySecret !== null;
    const { version: _version, source, ...serviceColumns } = saved.config;
    void _version;
    yield* transaction
      .update(schemaService)
      .set({
        ...serviceColumns,
        sourceType: source.type,
        sourceConfig: source,
        hasRegistryCredential,
        deletedAt: null,
        updatedAt: new Date(),
      })
      .where(
        and(
          eq(schemaService.environmentId, input.environmentId),
          eq(schemaService.id, saved.id),
        ),
      );

    yield* transaction
      .delete(schemaVariable)
      .where(eq(schemaVariable.serviceId, saved.id));
    for (const variable of saved.variables) {
      const configKeyId = yield* ensureConfigKey(transaction, {
        projectId: input.projectId,
        lineageId: input.currentLineageId,
        key: variable.key,
      });
      const { encryptedValue, ...valueColumns } = savedVariableColumns(variable);
      const [created] = yield* transaction
        .insert(schemaVariable)
        .values({
          id: variable.id,
          organizationId: input.organizationId,
          projectId: input.projectId,
          serviceId: saved.id,
          variableGroupId: null,
          configKeyId,
          key: variable.key,
          description: variable.description,
          exported: variable.exported,
          valueFingerprint: variable.valueFingerprint,
          ...valueColumns,
        })
        .returning({ id: schemaVariable.id });
      if (!created) {
        return yield* Effect.fail(
          new Error("Saved Service variable was not restored."),
        );
      }
      if (encryptedValue) {
        yield* transaction.insert(schemaVariableSecret).values({
          variableId: created.id,
          encryptedValue,
        });
      }
      yield* transaction
        .insert(schemaConfigValue)
        .values({
          projectId: input.projectId,
          configKeyId,
          environmentId: input.environmentId,
        })
        .onConflictDoUpdate({
          target: [
            schemaConfigValue.configKeyId,
            schemaConfigValue.environmentId,
          ],
          set: { updatedAt: new Date() },
        });
    }

    yield* transaction
      .delete(schemaServiceVariableGroupAttachment)
      .where(eq(schemaServiceVariableGroupAttachment.serviceId, saved.id));
    if (saved.variableGroupAttachments.length > 0) {
      yield* transaction.insert(schemaServiceVariableGroupAttachment).values(
        saved.variableGroupAttachments.map((attachment) => ({
          organizationId: input.organizationId,
          serviceId: saved.id,
          variableGroupId: attachment.variableGroupId,
          sortOrder: attachment.sortOrder,
        })),
      );
    }

    yield* transaction
      .delete(schemaServiceVolumeAttachment)
      .where(eq(schemaServiceVolumeAttachment.serviceId, saved.id));
    if (saved.volumeAttachments.length > 0) {
      yield* transaction.insert(schemaServiceVolumeAttachment).values(
        saved.volumeAttachments.map((attachment) => ({
          organizationId: input.organizationId,
          projectId: input.projectId,
          environmentId: input.environmentId,
          serviceId: saved.id,
          volumeResourceId: attachment.volumeResourceId,
          mountPath: attachment.mountPath,
        })),
      );
    }

    yield* transaction
      .delete(schemaServiceRegistryCredential)
      .where(eq(schemaServiceRegistryCredential.serviceId, saved.id));
    if (hasRegistryCredential) {
      yield* transaction.insert(schemaServiceRegistryCredential).values({
        serviceId: saved.id,
        encryptedRegistryUsername: saved.encryptedRegistryUsername,
        encryptedRegistrySecret: saved.encryptedRegistrySecret,
      });
    }

    return {
      variables: currentVariables.length > 0 || saved.variables.length > 0,
      variableGroupAttachments:
        currentVariableGroupAttachments.length > 0 ||
        saved.variableGroupAttachments.length > 0,
      volumeAttachments:
        currentVolumeAttachments.length > 0 ||
        saved.volumeAttachments.length > 0,
    } satisfies ServiceWorkingIntentResetCollections;
  });
}

export function restoreServiceWorkingIntentWithExecutor(
  input: {
    organizationId: string;
    projectId: string;
    environmentId: string;
    serviceId: string;
    savedStateSnapshotId: string;
  },
  transaction: ServiceTransaction,
) {
  return Effect.gen(function* () {
    const savedState = yield* loadEnvironmentSavedIntentById({
      environmentId: input.environmentId,
      savedStateSnapshotId: input.savedStateSnapshotId,
    });
    const saved = savedState?.intent.services.find(
      (service) => service.id === input.serviceId,
    );
    if (!saved) {
      return yield* new ServiceWorkingIntentNotFound({
        message: "Saved Service Working Intent not found.",
      });
    }
    const [current] = yield* transaction
      .select({ lineageId: schemaService.lineageId })
      .from(schemaService)
      .where(
        and(
          eq(schemaService.environmentId, input.environmentId),
          eq(schemaService.id, input.serviceId),
        ),
      )
      .limit(1);
    if (!current) {
      return yield* new ServiceWorkingIntentNotFound({
        message: "Service not found.",
      });
    }
    if (current.lineageId !== saved.lineageId) {
      return yield* new ServiceWorkingIntentConflict({
        message: "The Saved Service identity no longer matches Working State.",
      });
    }
    const nodeNames = yield* listEnvironmentNodeNames(
      input.environmentId,
      transaction,
    );
    if (
      isEnvironmentNodeNameTaken(saved.config.name, nodeNames, {
        type: "service",
        id: saved.id,
      })
    ) {
      return yield* new ServiceWorkingIntentConflict({
        message: getDuplicateEnvironmentNodeNameMessage(saved.config.name),
      });
    }
    return yield* replaceServiceWorkingIntent({
      ...input,
      currentLineageId: current.lineageId,
      saved,
      transaction,
    });
  }).pipe(
    Effect.mapError((cause) =>
      cause instanceof ServiceWorkingIntentNotFound ||
      cause instanceof ServiceWorkingIntentConflict ||
      cause instanceof ServiceWorkingIntentPersistenceFailure
        ? cause
        : cause instanceof Conflict
          ? new ServiceWorkingIntentConflict({ message: cause.message })
          : new ServiceWorkingIntentPersistenceFailure({ cause }),
    ),
  );
}

export const restoreServiceWorkingIntent = Effect.fn(
  "Services.restoreServiceWorkingIntent",
)(function* (actor: Actor, input: RestoreServiceWorkingIntentInput) {
  const parsed = yield* Effect.try({
    try: () => decodeStrict(restoreServiceWorkingIntentSchema, input),
    catch: () =>
      new ServiceWorkingIntentConflict({ message: "Invalid Service restore command." }),
  });
  const context = yield* getEnvironmentContextForActorById(actor, {
    organizationSlug: parsed.organizationSlug,
    environmentId: parsed.environmentId,
  });
  if (!context) {
    return yield* new ServiceWorkingIntentNotFound({
      message: "Environment not found.",
    });
  }
  const database = yield* Database;
  return yield* database.transaction(
    Effect.gen(function* () {
      const transaction = (yield* Database).drizzle;
      const changedCollections = yield* restoreServiceWorkingIntentWithExecutor(
        {
          organizationId: context.organization.id,
          projectId: context.project.id,
          environmentId: parsed.environmentId,
          serviceId: parsed.serviceId,
          savedStateSnapshotId: parsed.savedStateSnapshotId,
        },
        transaction,
      );
      const rows = yield* transaction.execute<{ txid: string }>(
        sql`select pg_current_xact_id()::xid::text as txid`,
        "objects",
      );
      const txid = Number(rows[0]?.txid);
      if (!Number.isSafeInteger(txid)) {
        return yield* Effect.fail(
          new Error("Postgres did not return a valid transaction ID."),
        );
      }
      return { data: { changedCollections }, txid };
    }),
  ).pipe(
    Effect.mapError((cause) =>
      cause instanceof ServiceWorkingIntentNotFound ||
      cause instanceof ServiceWorkingIntentConflict ||
      cause instanceof ServiceWorkingIntentPersistenceFailure
        ? cause
        : new ServiceWorkingIntentPersistenceFailure({ cause }),
    ),
  );
});
