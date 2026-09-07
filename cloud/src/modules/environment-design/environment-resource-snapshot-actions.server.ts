import "@tanstack/react-start/server-only";

import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import {
  environmentResource as schemaEnvironmentResource,
  environmentVariableGroup as schemaEnvironmentVariableGroup,
  variable as schemaVariable,
  environmentCanvasNodePosition as schemaEnvironmentCanvasNodePosition,
  resourceLineage as schemaResourceLineage,
} from "#/modules/environment-design/tables";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import {
  requireEnvironmentForActorById,
} from "#/modules/environment-design/authoring-repository.server";
import {
  loadAuthorizedEnvironmentNodeSnapshotConfig,
} from "#/modules/environment-design/environment-node-snapshot-config.server";
import {
  getVariableGroupResource,
  getVolumeResource,
} from "#/modules/environment-design/resource-repository.server";
import type {
  DiscardVolumeResourceInput,
  RestoreVariableGroupResourceSnapshotInput,
} from "#/modules/environment-design/resources";
import {
  createOwnedVariable,
  listVariableRowsForGroup,
  updateOwnedVariable,
} from "#/modules/environment-design/variable-repository.server";
import { parseDisplayToParts } from "#/modules/environment-design/variable-template";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { getPlainVariableValueFingerprint } from "#/utils/encrypted-secret.server";

function plainSnapshotValue(value: string) {
  const { parts } = parseDisplayToParts(value, () => null);
  return {
    valueKind: "plain" as const,
    valueParts: parts,
    encryptedValue: null,
    valueFingerprint: getPlainVariableValueFingerprint(JSON.stringify(parts)),
  };
}

export const restoreVariableGroupResourceSnapshot = Effect.fn(
  "EnvironmentDesign.restoreVariableGroupResourceSnapshot",
)(function* (
  actor: Actor,
  input: RestoreVariableGroupResourceSnapshotInput,
) {
  const access = yield* requireEnvironmentForActorById(actor, input);
  const row = yield* getVariableGroupResource(input.environmentId, input.resourceId);
  if (!row) {
    return yield* new NotFound({
      message: "Variable Group not found.",
    });
  }
  const config = yield* loadAuthorizedEnvironmentNodeSnapshotConfig({
    environmentId: input.environmentId,
    nodeType: "variable_group",
    nodeId: input.resourceId,
    snapshotSource: input.snapshotSource,
  });

  const receipt = yield* withMutationReceipt(
    Effect.gen(function* () {
      const database = yield* Database;
      const projectId = access.project.id;
      const variableGroupId = row.variableGroup.id;
      yield* database.drizzle
        .update(schemaEnvironmentResource)
        .set({ name: config.name, updatedAt: new Date() })
        .where(eq(schemaEnvironmentResource.id, row.resource.id));
      yield* database.drizzle
        .update(schemaEnvironmentVariableGroup)
        .set({ name: config.name, updatedAt: new Date() })
        .where(eq(schemaEnvironmentVariableGroup.id, variableGroupId));

      const currentVariables = yield* listVariableRowsForGroup(variableGroupId);
      const currentByKey = new Map(
        currentVariables.map((variable) => [variable.key, variable]),
      );
      const snapshotKeys = new Set(config.variables.map((variable) => variable.key));
      const orphanIds = currentVariables.flatMap((variable) =>
        snapshotKeys.has(variable.key) ? [] : [variable.id],
      );
      if (orphanIds.length > 0) {
        yield* database.drizzle
          .delete(schemaVariable)
          .where(inArray(schemaVariable.id, orphanIds));
      }

      for (const variable of config.variables) {
        const existing = currentByKey.get(variable.key);
        let storedValue;
        if (variable.value.type === "plain") {
          storedValue = plainSnapshotValue(variable.value.value);
        } else {
          const encryptedValue = variable.value.encryptedValue;
          if (encryptedValue === undefined) {
            return yield* new NotFound({
              message: "Variable secret snapshot not found.",
            });
          }
          storedValue = {
            valueKind: "sealed" as const,
            valueParts: null,
            encryptedValue,
            valueFingerprint: variable.value.fingerprint,
          };
        }
        const values = {
          key: variable.key,
          description: variable.description,
          exported: variable.exported,
          ...storedValue,
        };
        if (existing) {
          yield* updateOwnedVariable({
            projectId,
            environmentId: input.environmentId,
            row: existing,
            values,
          });
        } else {
          yield* createOwnedVariable({
            projectId,
            environmentId: input.environmentId,
            owner: {
              scope: "variable_group_lineage",
              lineageId: row.variableGroup.lineageId,
            },
            values: {
              serviceId: null,
              variableGroupId,
              ...values,
            },
          });
        }
      }
    }),
  );

  const restored = yield* getVariableGroupResource(input.environmentId, input.resourceId);
  if (!restored) {
    return yield* new NotFound({
      message: "Variable Group not found.",
    });
  }
  return { ...receipt, data: restored };
});

export const discardVolumeResource = Effect.fn(
  "EnvironmentDesign.discardVolumeResource",
)(function* (actor: Actor, input: DiscardVolumeResourceInput) {
  yield* requireEnvironmentForActorById(actor, input);
  const current = yield* getVolumeResource(input.environmentId, input.resourceId);
  if (!current) {
    return yield* new NotFound({ message: "Volume not found." });
  }

  if (input.snapshotSource) {
    const config = yield* loadAuthorizedEnvironmentNodeSnapshotConfig({
      environmentId: input.environmentId,
      nodeType: "volume",
      nodeId: input.resourceId,
      snapshotSource: input.snapshotSource,
    });
    const receipt = yield* withMutationReceipt(
      Effect.gen(function* () {
        const database = yield* Database;
        yield* database.drizzle
          .update(schemaEnvironmentResource)
          .set({ name: config.name, deletedAt: null, updatedAt: new Date() })
          .where(eq(schemaEnvironmentResource.id, current.resource.id));
      }),
    );
    return { ...receipt, data: { resourceId: input.resourceId } };
  }

  const receipt = yield* withMutationReceipt(
    Effect.gen(function* () {
      const database = yield* Database;
      if (current.resource.deletedAt !== null) {
        yield* database.drizzle
          .update(schemaEnvironmentResource)
          .set({ deletedAt: null, updatedAt: new Date() })
          .where(eq(schemaEnvironmentResource.id, current.resource.id));
        return;
      }
      yield* database.drizzle
        .delete(schemaEnvironmentCanvasNodePosition)
        .where(
          and(
            eq(schemaEnvironmentCanvasNodePosition.resourceType, "volume"),
            eq(schemaEnvironmentCanvasNodePosition.resourceId, input.resourceId),
          ),
        );
      yield* database.drizzle
        .delete(schemaEnvironmentResource)
        .where(eq(schemaEnvironmentResource.id, input.resourceId));
      yield* database.drizzle
        .delete(schemaResourceLineage)
        .where(eq(schemaResourceLineage.id, current.resource.lineageId));
    }),
  );
  return { ...receipt, data: { resourceId: input.resourceId } };
});
