import { volumeIsAuthored } from "#/modules/environment-design/document-identity.server";
import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import {
  environmentResource as schemaEnvironmentResource,
} from "#/modules/environment-design/tables";
import {
  DestructiveVolumeEnvironmentNotFound,
  DestructiveVolumePersistenceFailure,
  DestructiveVolumeProviderFailure,
} from "#/modules/operations/destructive-volume-errors";
import { getVolumePhysicalName } from "#/modules/environment-design/volume-config";
import {
  prepareNamespaceDestructiveEvidence,
  type PreparedNamespaceDestructiveEvidence,
} from "#/modules/operations/destructive-volume-evidence";
import {
  OrganizationRuntime,
  type ConnectedRuntimeClient,
} from "#/modules/runtime/organization-runtime.server";
import { CLUSTER_UNREACHABLE_ERROR } from "#/modules/runtime/runtime.collection";
import {
  runtimeVolumeSnapshotFromWatch,
  type RuntimeVolumeSnapshot,
} from "#/modules/runtime/runtime-volume";
import {
  getEnvironmentContextForActorById,
} from "#/modules/environment-design/authoring-repository.server";
import { Database } from "#/server/database.server";
import type { Actor } from "#/modules/identity/actor";

export const DESTRUCTIVE_VOLUME_WATCH_TIMEOUT_MS = 5_000;

export type DestructiveVolumeActionErrorReason =
  | "no_connection"
  | "query_failed"
  | "volume_duplicate";

export class DestructiveVolumeActionError extends Data.TaggedError(
  "DestructiveVolumeActionError",
)<{
  reason: DestructiveVolumeActionErrorReason;
  message: string;
}> {}

type ActionError =
  | DestructiveVolumeEnvironmentNotFound
  | DestructiveVolumePersistenceFailure
  | DestructiveVolumeProviderFailure
  | DestructiveVolumeActionError;

type AuthorizedEnvironment = {
  organizationId: string;
  environmentId: string;
  namespaceId: string;
};

type RuntimeSession = {
  connected: ConnectedRuntimeClient;
};

export type NamespaceActionIdentity = {
  actor: Actor;
  organizationSlug: string;
  environmentId: string;
};

export type DestructiveVolumeActionDeps<
  RAuthorize = Database,
  RRuntime = OrganizationRuntime,
  RList = Database,
> = {
  authorizeNamespace(
    input: NamespaceActionIdentity,
  ): Effect.Effect<AuthorizedEnvironment, ActionError, RAuthorize>;
  useRuntime<T, E, R>(
    organizationId: string,
    use: (session: RuntimeSession) => Effect.Effect<T, E, R>,
  ): Effect.Effect<T, ActionError | E, R | RRuntime>;
  listEnvironmentVolumeNames(
    environmentId: string,
  ): Effect.Effect<Set<string>, DestructiveVolumePersistenceFailure, RList>;
};

export type PreparedNamespaceDestruction = AuthorizedEnvironment & {
  evidence: PreparedNamespaceDestructiveEvidence;
};

export function createDestructiveVolumeActions<RAuthorize, RRuntime, RList>(
  deps: DestructiveVolumeActionDeps<RAuthorize, RRuntime, RList>,
) {
  return {
    prepareNamespace: Effect.fn("Operations.prepareDestructiveVolumeNamespace")(
      function* (input: NamespaceActionIdentity) {
        const authorization = yield* deps.authorizeNamespace(input);
        return yield* deps.useRuntime(
          authorization.organizationId,
          (session) =>
            Effect.gen(function* () {
              const names = yield* deps.listEnvironmentVolumeNames(
                authorization.environmentId,
              );
              return yield* gatherNamespace(session, authorization, names);
            }),
        );
      },
    ),
  };
}

function gatherNamespace(
  session: RuntimeSession,
  authorization: AuthorizedEnvironment,
  environmentVolumeNames: ReadonlySet<string>,
): Effect.Effect<PreparedNamespaceDestruction, DestructiveVolumeActionError> {
  return Effect.gen(function* () {
    const listed = yield* listVolumes(session);
    const matching = listed.filter((snapshot) =>
      environmentVolumeNames.has(snapshot.name),
    );
    const duplicate = findDuplicateVolumeName(matching);
    if (duplicate) {
      return yield* actionError(
        "volume_duplicate",
        `Runtime reported volume ${duplicate} more than once in namespace ${authorization.namespaceId}.`,
      );
    }
    return {
      ...authorization,
      evidence: prepareNamespaceDestructiveEvidence(
        authorization.namespaceId,
        matching,
      ),
    };
  });
}

function listVolumes(
  session: RuntimeSession,
): Effect.Effect<RuntimeVolumeSnapshot[], DestructiveVolumeActionError> {
  return session.connected.watchFirstFrame(DESTRUCTIVE_VOLUME_WATCH_TIMEOUT_MS).pipe(
    Effect.map((frame) => frame.volumes.map(runtimeVolumeSnapshotFromWatch)),
    Effect.mapError(
      (cause) =>
        new DestructiveVolumeActionError({
          reason: "query_failed",
          message: `gather volume testimony failed: ${cause.message}`,
        }),
    ),
  );
}

function findDuplicateVolumeName(snapshots: readonly RuntimeVolumeSnapshot[]) {
  const seen = new Set<string>();
  for (const snapshot of snapshots) {
    if (seen.has(snapshot.name)) return snapshot.name;
    seen.add(snapshot.name);
  }
  return null;
}

function actionError(
  reason: DestructiveVolumeActionErrorReason,
  message: string,
): DestructiveVolumeActionError {
  return new DestructiveVolumeActionError({ reason, message });
}

function authorizeEnvironment(
  input: NamespaceActionIdentity,
): Effect.Effect<AuthorizedEnvironment, ActionError, Database> {
  return getEnvironmentContextForActorById(
          input.actor,
          {
            organizationSlug: input.organizationSlug,
            environmentId: input.environmentId,
          },
        )
    .pipe(
      Effect.mapError(
        (cause) =>
          new DestructiveVolumeProviderFailure({ cause }),
      ),
      Effect.flatMap((context) =>
        context
          ? Effect.succeed({
              organizationId: context.organization.id,
              environmentId: context.environment.id,
              namespaceId: context.environment.namespace,
            })
          : Effect.fail(
              new DestructiveVolumeEnvironmentNotFound({
                environmentId: input.environmentId,
              }),
            ),
      ),
    );
}

function listEnvironmentVolumeNames(environmentId: string) {
  return Effect.gen(function* () {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select({ id: schemaEnvironmentResource.id })
      .from(schemaEnvironmentResource)
      .where(
        and(
          eq(schemaEnvironmentResource.environmentId, environmentId),
          eq(schemaEnvironmentResource.implementationType, "volume"),
          volumeIsAuthored,
        ),
      )
      .pipe(
        Effect.mapError(
          (cause) =>
            new DestructiveVolumePersistenceFailure({ cause }),
        ),
      );
    return new Set(rows.map((row) => getVolumePhysicalName(row.id)));
  });
}

const productionDeps: DestructiveVolumeActionDeps = {
  authorizeNamespace: authorizeEnvironment,
  useRuntime(organizationId, use) {
    return Effect.scoped(
        Effect.gen(function* () {
          const runtime = yield* OrganizationRuntime;
          const session = yield* runtime.open(organizationId).pipe(
            Effect.mapError(
              (cause) =>
              new DestructiveVolumeProviderFailure({ cause }),
            ),
          );
          switch (session.status) {
            case "connected":
              return yield* use({ connected: session.connected });
            case "no_connection":
              return yield* actionError(
                "no_connection",
                "This organization does not have a Cloud runtime connection.",
              );
            case "unreachable":
              return yield* actionError(
                "no_connection",
                CLUSTER_UNREACHABLE_ERROR,
              );
          }
        }),
    );
  },
  listEnvironmentVolumeNames,
};

export const destructiveVolumeActions =
  createDestructiveVolumeActions(productionDeps);
