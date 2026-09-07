import "@tanstack/react-start/server-only";

import { eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import {
  environmentResource as schemaEnvironmentResource,
  service as schemaService,
} from "#/modules/environment-design/tables";
import { machineEnrollmentToken as schemaMachineEnrollmentToken } from "#/modules/machines/tables";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";
import { organization as schemaOrganization } from "#/modules/organization/tables";
import {
  environment as schemaEnvironment,
  project as schemaProject,
} from "#/modules/project/tables";
import { organizationPairing as schemaOrganizationPairing } from "#/modules/runtime/tables";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import { tryRevokeOrganizationRelayPairing } from "#/modules/machines/enrollment.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import {
  PloyzProviderError,
  type PloyzSdkError,
  type PloyzSession,
} from "#/modules/runtime/ployz.server";
import {
  confirmedVolumesForTeardown,
  leftoverVolumeMessage,
  teardownIsTerminal,
  TeardownIncompleteError,
  type TeardownEnvironmentTarget,
} from "#/modules/runtime/teardown";
import {
  claimTeardownAttempt,
  completeTeardownAttempt,
  loadTeardownAttempt,
  loadTeardownAttemptByRun,
  type TeardownAttempt,
} from "#/modules/runtime/teardown.repository";
import { parseVolumeRemoveOutcome } from "#/modules/runtime/volume-removal-outcome";
import type {
  VolumeRemoveOutcome,
} from "#/modules/runtime/volume-removal";
import { Database } from "#/server/database.server";

export function composeEnvironmentDestroy(
  session: PloyzSession,
  input: {
    namespace: string;
    identities: readonly DataLossIdentity[];
  },
): Effect.Effect<
  VolumeRemoveOutcome | null,
  PloyzSdkError | TeardownIncompleteError
> {
  return Effect.gen(function* () {
    const destroyed = yield* session
      .destroyProject(
        input.namespace,
        { confirmed: [...input.identities] },
        true,
      )
      .pipe(
        Effect.as(true),
        Effect.catchTag("SdkSurfaceNotShipped", () => Effect.succeed(false)),
      );
    if (destroyed) return null;
    const volumes = confirmedVolumesForTeardown(input.identities);
    if (volumes.length === 0) return null;
    const outcome = yield* session.removeVolumes({ volumes: [...volumes], force: false }).pipe(
      Effect.map((raw) => parseVolumeRemoveOutcome(raw, volumes)),
    );
    const leftover = leftoverVolumeMessage(volumes, outcome);
    if (leftover !== null) {
      return yield* Effect.fail(new TeardownIncompleteError(leftover));
    }
    return outcome;
  });
}

export const loadTeardownAttemptActivity = Effect.fn("Teardown.loadAttempt")(
  loadTeardownAttempt,
);

export const claimTeardownAttemptActivity = Effect.fn("Teardown.claim")(
  (input: Parameters<typeof claimTeardownAttempt>[0]) =>
    claimTeardownAttempt(input),
);

export const completeTeardownAttemptActivity = Effect.fn("Teardown.complete")(
  (input: Parameters<typeof completeTeardownAttempt>[0]) =>
    completeTeardownAttempt(input),
);

export const prepareTeardownAttemptActivity = Effect.fn("Teardown.prepare")(
  function* (input: {
    readonly attemptId: string;
    readonly inngestRunId: string;
    readonly now: Date;
  }) {
    const existing = yield* loadTeardownAttemptActivity(input.attemptId);
    if (existing === null) return { kind: "missing" as const };
    if (teardownIsTerminal(existing.status)) {
      return { kind: "terminal" as const, attempt: existing };
    }
    const claimed = yield* claimTeardownAttemptActivity(input);
    return { kind: "ready" as const, attempt: claimed.attempt };
  },
);

export const destroyEnvironmentActivity = Effect.fn(
  "Teardown.destroyEnvironment",
)(function* (input: {
  readonly organizationId: string;
  readonly target: TeardownEnvironmentTarget;
}) {
  const runtime = yield* OrganizationRuntime;
  const session = yield* runtime.open(input.organizationId);
  if (session.status !== "connected") {
    return yield* new PloyzProviderError({
      operation: "open organization runtime",
      cause: session,
    });
  }
  return yield* composeEnvironmentDestroy(session.connected, {
    namespace: input.target.namespace,
    identities: input.target.identities,
  });
});

export const removeTeardownMachineActivity = Effect.fn(
  "Teardown.removeMachine",
)(function* (input: {
  readonly organizationId: string;
  readonly machineId: string;
  readonly identities: readonly DataLossIdentity[];
}) {
  const runtime = yield* OrganizationRuntime;
  const session = yield* runtime.open(input.organizationId);
  if (session.status !== "connected") {
    return yield* new PloyzProviderError({
      operation: "open organization runtime",
      cause: session,
    });
  }
  yield* session.connected.removeMachine(input.machineId, {
    confirmed: [...input.identities],
  });
});

export const revokeTeardownPairingActivity = Effect.fn(
  "Teardown.revokePairing",
)(function* (input: {
  readonly organizationId: string;
  readonly requireComplete: boolean;
}) {
  const revoked = yield* tryRevokeOrganizationRelayPairing(
    input.organizationId,
  );
  if (!revoked && input.requireComplete) {
    return yield* new PloyzProviderError({
      operation: "revoke Relay pairing",
      cause: new TeardownIncompleteError(
        "Relay pairing revocation did not complete.",
      ),
    });
  }
  if (!revoked) return { rustMustRevokePairing: true as const };
  const database = yield* Database;
  yield* Effect.all([
    database.drizzle
      .delete(schemaOrganizationPairing)
      .where(
        eq(schemaOrganizationPairing.organizationId, input.organizationId),
      ),
    database.drizzle
      .delete(schemaMachineEnrollmentToken)
      .where(
        eq(schemaMachineEnrollmentToken.organizationId, input.organizationId),
      ),
  ]);
  return { rustMustRevokePairing: false as const };
});

export const dropTeardownCloudRowsActivity = Effect.fn(
  "Teardown.dropCloudRows",
)(function* (attempt: TeardownAttempt) {
  const environmentIds = [
    ...new Set(
      attempt.targets.environments.map((environment) => environment.environmentId),
    ),
  ];
  const projectIds = [
    ...new Set([
      ...attempt.targets.environments.map((environment) => environment.projectId),
      ...(attempt.projectId === null ? [] : [attempt.projectId]),
    ]),
  ];
  const database = yield* Database;
  yield* database.transaction(
    Effect.gen(function* () {
      const transaction = yield* Database;
      if (environmentIds.length > 0) {
        const deployments = yield* transaction.drizzle
          .select({ id: schemaEnvironmentDeployment.id })
          .from(schemaEnvironmentDeployment)
          .where(inArray(schemaEnvironmentDeployment.environmentId, environmentIds));
        const deploymentIds = deployments.map((row) => row.id);
        if (deploymentIds.length > 0) {
          yield* transaction.drizzle
            .delete(schemaDestructiveVolumeAttempt)
            .where(
              inArray(
                schemaDestructiveVolumeAttempt.environmentDeploymentId,
                deploymentIds,
              ),
            );
        }
        yield* transaction.drizzle
          .update(schemaEnvironmentDeployment)
          .set({ retryOfDeploymentId: null })
          .where(inArray(schemaEnvironmentDeployment.environmentId, environmentIds));
        yield* transaction.drizzle
          .delete(schemaEnvironmentResource)
          .where(inArray(schemaEnvironmentResource.environmentId, environmentIds));
        yield* transaction.drizzle
          .delete(schemaService)
          .where(inArray(schemaService.environmentId, environmentIds));
        yield* transaction.drizzle
          .delete(schemaEnvironment)
          .where(inArray(schemaEnvironment.id, environmentIds));
      }
      if (
        (attempt.scope === "project" || attempt.scope === "organization") &&
        projectIds.length > 0
      ) {
        yield* transaction.drizzle
          .delete(schemaProject)
          .where(inArray(schemaProject.id, projectIds));
      }
      if (attempt.scope === "organization") {
        yield* transaction.drizzle
          .delete(schemaOrganizationPairing)
          .where(eq(schemaOrganizationPairing.organizationId, attempt.organizationId));
        yield* transaction.drizzle
          .delete(schemaMachineEnrollmentToken)
          .where(eq(schemaMachineEnrollmentToken.organizationId, attempt.organizationId));
        yield* transaction.drizzle
          .delete(schemaOrganization)
          .where(eq(schemaOrganization.id, attempt.organizationId));
      }
    }),
  );
});

export const failOwnedTeardownAttemptActivity = Effect.fn(
  "fail-teardown-retry-exhausted",
)(function* (input: {
  readonly attemptId: string;
  readonly inngestRunId: string;
  readonly failureMessage: string;
  readonly now: Date;
}) {
  const attempt = yield* loadTeardownAttemptActivity(input.attemptId);
  if (
    attempt === null ||
    attempt.status !== "running" ||
    attempt.inngestRunId !== input.inngestRunId
  ) {
    return { state: "skipped" as const };
  }
  yield* completeTeardownAttemptActivity({
    attemptId: attempt.id,
    inngestRunId: input.inngestRunId,
    status: "failed",
    failureMessage: input.failureMessage,
    now: input.now,
  });
  return { state: "failed" as const };
});

export const cancelTeardownAttemptActivity = Effect.fn(
  "Teardown.cancelOwned",
)(function* (input: { readonly inngestRunId: string; readonly now: Date }) {
  const attempt = yield* loadTeardownAttemptByRun(input.inngestRunId);
  if (attempt === null || attempt.status !== "running") {
    return { state: "skipped" as const };
  }
  yield* completeTeardownAttemptActivity({
    attemptId: attempt.id,
    inngestRunId: input.inngestRunId,
    status: "cancelled",
    failureMessage: "Teardown was cancelled.",
    now: input.now,
  });
  return { state: "cancelled" as const };
});
