import "@tanstack/react-start/server-only";

import { and, eq } from "drizzle-orm";
import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { Effect } from "effect";
import {
  environmentSavedStateSnapshot as schemaEnvironmentSavedStateSnapshot,
} from "#/modules/deployments/tables";
import {
  environmentNodeConfigSnapshot as schemaEnvironmentNodeConfigSnapshot,
} from "#/modules/runtime/tables";
import {
  decodeEnvironmentResourceNodeConfig,
  getEnvironmentResourceNodeSnapshotResourceName,
  type EnvironmentResourceNodeConfigByType,
  type EnvironmentResourceNodeType,
} from "#/modules/environment-design/environment-resource-node";
import type {
  EnvironmentSnapshotSource,
} from "#/modules/environment-design/environment-snapshot-source";
import { decodePersistedSavedEnvironmentState } from "#/modules/environment-design/saved-intent";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

type SnapshotConfigInput = {
  environmentId: string;
  nodeId: string;
  snapshotSource: EnvironmentSnapshotSource;
};

function snapshotNotFound(input: {
  nodeType: EnvironmentResourceNodeType;
  nodeId: string;
}) {
  return new NotFound({
    message: `${getEnvironmentResourceNodeSnapshotResourceName(input.nodeType)} not found.`,
  });
}

type SnapshotConfigFailure = NotFound | Conflict | EffectDrizzleQueryError;

export function loadAuthorizedEnvironmentNodeSnapshotConfig(
  input: SnapshotConfigInput & { nodeType: "variable_group" },
): Effect.Effect<
  EnvironmentResourceNodeConfigByType["variable_group"],
  SnapshotConfigFailure,
  Database
>;
export function loadAuthorizedEnvironmentNodeSnapshotConfig(
  input: SnapshotConfigInput & { nodeType: "volume" },
): Effect.Effect<
  EnvironmentResourceNodeConfigByType["volume"],
  SnapshotConfigFailure,
  Database
>;
export function loadAuthorizedEnvironmentNodeSnapshotConfig(
  input: SnapshotConfigInput & { nodeType: EnvironmentResourceNodeType },
): Effect.Effect<
  EnvironmentResourceNodeConfigByType[EnvironmentResourceNodeType],
  SnapshotConfigFailure,
  Database
> {
  return Effect.gen(function* () {
    const database = yield* Database;
    let config: unknown;
    if (input.snapshotSource.kind === "deployment") {
      const rows = yield* database.drizzle
        .select({ config: schemaEnvironmentNodeConfigSnapshot.config })
        .from(schemaEnvironmentNodeConfigSnapshot)
        .where(
          and(
            eq(
              schemaEnvironmentNodeConfigSnapshot.environmentDeploymentId,
              input.snapshotSource.environmentDeploymentId,
            ),
            eq(
              schemaEnvironmentNodeConfigSnapshot.environmentId,
              input.environmentId,
            ),
            eq(schemaEnvironmentNodeConfigSnapshot.nodeType, input.nodeType),
            eq(schemaEnvironmentNodeConfigSnapshot.nodeId, input.nodeId),
          ),
        )
        .limit(1);
      if (!rows[0]) return yield* snapshotNotFound(input);
      config = rows[0].config;
    } else {
      const rows = yield* database.drizzle
        .select({ intent: schemaEnvironmentSavedStateSnapshot.intent })
        .from(schemaEnvironmentSavedStateSnapshot)
        .where(
          and(
            eq(
              schemaEnvironmentSavedStateSnapshot.id,
              input.snapshotSource.environmentSavedStateSnapshotId,
            ),
            eq(
              schemaEnvironmentSavedStateSnapshot.environmentId,
              input.environmentId,
            ),
          ),
        )
        .limit(1);
      const saved = rows[0];
      if (!saved) return yield* snapshotNotFound(input);
      const nodes = (
        yield* decodePersistedSavedEnvironmentState({
          environmentId: input.environmentId,
          ...saved,
        })
      ).nodeSnapshots;
      const node = nodes.find(
        (snapshot) =>
          snapshot.nodeType === input.nodeType && snapshot.nodeId === input.nodeId,
      );
      if (!node) return yield* snapshotNotFound(input);
      config = node.config;
    }

    const invalidConfig = () =>
      new Conflict({
        message: "Environment resource snapshot config is invalid.",
      });
    return yield* decodeEnvironmentResourceNodeConfig(input.nodeType, config).pipe(
      Effect.map((decoded) => decoded.config),
      Effect.mapError(invalidConfig),
    );
  });
}
