import { volumeIsAuthored } from "#/modules/environment-design/document-identity.server";
import { getVolumeResource } from "#/modules/environment-design/resource-repository.server";
import "@tanstack/react-start/server-only";

import type { MachineId } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Cause, Effect, Exit } from "effect";
import { describeFailureCause } from "#/lib/error-message";
import {
  environmentResource as schemaEnvironmentResource,
  environmentCanvasNodePosition as schemaEnvironmentCanvasNodePosition,
} from "#/modules/environment-design/tables";
import { member as schemaMember } from "#/modules/identity/tables";
import { organization as schemaOrganization } from "#/modules/organization/tables";
import {
  environment as schemaEnvironment,
  project as schemaProject,
} from "#/modules/project/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createVolumeRemoveRequestedEvent } from "#/modules/inngest/events";
import { getVolumePhysicalName } from "#/modules/environment-design/volume-config";
import {
  directVolumeDataLoss,
  unionDataLossLists,
} from "#/modules/runtime/data-loss-confirm";
import {
  RUNTIME_VOLUME_REQUEST_TIMEOUT_MS,
  runtimeVolumeSnapshotFromWatch,
} from "#/modules/runtime/runtime-volume";
import type { Actor } from "#/modules/identity/actor";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import {
  PloyzProviderError,
  type PloyzSession,
} from "#/modules/runtime/ployz.server";
import {
  retryVolumesForAttempt,
  volumeRemoveIsTerminal,
  volumeRemoveStatusFromOutcome,
  volumesConfirmedForPhysicalName,
  type ConfirmVolumeRemoveInput,
  type RetryVolumeRemoveInput,
  type VolumeRemoveOutcome,
  type VolumeResourceInput,
} from "#/modules/runtime/volume-removal";
import { parseVolumeRemoveOutcome } from "#/modules/runtime/volume-removal-outcome";
import {
  claimVolumeRemoveAttempt,
  completeVolumeRemoveAttempt,
  beginVolumeRemoveAttempt,
  insertVolumeRemoveAttempt,
  loadLatestVolumeRemoveAttemptForResource,
  loadVolumeRemoveAttempt,
  loadVolumeRemoveAttemptByRun,
  markVolumeRemoveAttemptUnknown,
  type VolumeRemoveAttempt,
} from "#/modules/runtime/volume-removal.repository";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";

type EnvironmentAccess = {
  readonly organizationId: string;
  readonly environmentId: string;
};

type VolumeResource = EnvironmentAccess & {
  readonly resourceId: string;
  readonly resourceName: string;
};

const requireEnvironmentAccess = Effect.fn("VolumeRemoval.requireEnvironment")(
  function* (actor: Actor, input: { organizationSlug: string; environmentId: string }) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select({
        organizationId: schemaOrganization.id,
        environmentId: schemaEnvironment.id,
      })
      .from(schemaEnvironment)
      .innerJoin(schemaProject, eq(schemaEnvironment.projectId, schemaProject.id))
      .innerJoin(schemaOrganization, eq(schemaProject.organizationId, schemaOrganization.id))
      .innerJoin(
        schemaMember,
        and(
          eq(schemaMember.organizationId, schemaOrganization.id),
          eq(schemaMember.userId, actor.userId),
        ),
      )
      .where(
        and(
          eq(schemaEnvironment.id, input.environmentId),
          eq(schemaOrganization.slug, input.organizationSlug),
        ),
      )
      .limit(1);
    const access = rows[0];
    if (access !== undefined) return access;
    return yield* new NotFound({
      message: "The environment was not found.",
    });
  },
);

const requireTombstonedVolume = Effect.fn("VolumeRemoval.requireVolume")(
  function* (actor: Actor, input: VolumeResourceInput) {
    const access = yield* requireEnvironmentAccess(actor, input);
    const { drizzle } = yield* Database;
    const [authority] = yield* drizzle
      .select({ isAuthored: volumeIsAuthored })
      .from(schemaEnvironmentResource)
      .where(
        and(
          eq(schemaEnvironmentResource.id, input.resourceId),
          eq(schemaEnvironmentResource.environmentId, access.environmentId),
          eq(schemaEnvironmentResource.organizationId, access.organizationId),
          eq(schemaEnvironmentResource.implementationType, "volume"),
        ),
      )
      .limit(1);
    if (authority === undefined) {
      return yield* new NotFound({ message: "The volume was not found." });
    }
    if (authority.isAuthored) {
      return yield* new Validation({
        field: "resourceId",
        message: "Stage deletion before removing volume data.",
      });
    }
    const view = yield* getVolumeResource(access.environmentId, input.resourceId);
    if (!view) return yield* new NotFound({ message: "The volume was not found." });
    return {
      ...access,
      resourceId: input.resourceId,
      resourceName: view.resource.name,
    } satisfies VolumeResource;
  },
);

export const dispatchVolumeRemoveRequested = Effect.fn(
  "VolumeRemoval.dispatchRequested",
)(function* (attemptId: string) {
  yield* sendInngestEvent(createVolumeRemoveRequestedEvent({ attemptId }));
});

function volumeDataLoss(session: PloyzSession, resource: VolumeResource) {
  return session.watchFirstFrame(RUNTIME_VOLUME_REQUEST_TIMEOUT_MS).pipe(
    Effect.map((frame) => {
      const physicalName = getVolumePhysicalName(resource.resourceId);
      const matching = frame.volumes
        .map(runtimeVolumeSnapshotFromWatch)
        .flatMap((volume) =>
          volume.name === physicalName
            ? [
                {
                  // SAFETY: Runtime observations carry SDK machine identities.
                  machine_id: volume.machine_id as MachineId,
                  name: volume.name,
                },
              ]
            : [],
        );
      return unionDataLossLists([
        directVolumeDataLoss(matching),
        { rust: [], cloud: [{ kind: "volume", name: resource.resourceName }] },
      ]);
    }),
  );
}

export const loadVolumeRemoveDataLoss = Effect.fn("VolumeRemoval.loadDataLoss")(
  function* (actor: Actor, input: VolumeResourceInput) {
    const resource = yield* requireTombstonedVolume(actor, input);
    const runtime = yield* OrganizationRuntime;
    const session = yield* runtime.open(resource.organizationId);
    if (session.status !== "connected") {
      return yield* new PloyzProviderError({
        operation: "open organization runtime",
        cause: session,
      });
    }
    return yield* volumeDataLoss(session.connected, resource);
  },
);

export const confirmVolumeRemove = Effect.fn("VolumeRemoval.confirm")(
  function* (actor: Actor, input: ConfirmVolumeRemoveInput) {
    const resource = yield* requireTombstonedVolume(actor, input);
    const confirmed = volumesConfirmedForPhysicalName(
      input.identities,
      getVolumePhysicalName(resource.resourceId),
    );
    if (confirmed.error === "name_mismatch") {
      return yield* new Validation({
        field: "identities",
        message: "Confirmed volumes must use this volume's machine name.",
      });
    }
    if (confirmed.error === "empty") {
      return yield* new Validation({
        field: "identities",
        message: "No machine volumes to remove.",
      });
    }
    const attempt = yield*
      insertVolumeRemoveAttempt({
        organizationId: resource.organizationId,
        requestedByUserId: actor.userId,
        environmentId: resource.environmentId,
        environmentResourceId: resource.resourceId,
        volumes: confirmed.volumes,
      });
    yield* dispatchVolumeRemoveRequested(attempt.id);
    return attempt;
  },
);

export const retryVolumeRemove = Effect.fn("VolumeRemoval.retry")(
  function* (actor: Actor, input: RetryVolumeRemoveInput) {
    const attempt = yield* loadVolumeRemoveAttempt(input.attemptId);
    if (attempt === null) {
      return yield* new NotFound({
        message: "The volume remove attempt was not found.",
      });
    }
    const access = yield* requireEnvironmentAccess(actor, {
      organizationSlug: input.organizationSlug,
      environmentId: attempt.environmentId,
    });
    if (attempt.organizationId !== access.organizationId) {
      return yield* new NotFound({
        message: "The volume remove attempt was not found.",
      });
    }
    const retry = retryVolumesForAttempt(attempt);
    if (retry.kind === "conflict") {
      return yield* new Conflict({
        message: "This volume remove cannot be retried yet.",
      });
    }
    if (retry.kind === "resend") {
      yield* dispatchVolumeRemoveRequested(attempt.id);
      return attempt;
    }
    if (
      attempt.environmentDeploymentId !== null &&
      attempt.inngestRunId === null
    ) {
      return yield* new Validation({
        field: "attemptId",
        message:
          "This volume removal was never released by its deployment. Submit a fresh reviewed deployment instead.",
      });
    }
    if (retry.volumes.length === 0) {
      return yield* new Validation({
        field: "attemptId",
        message: "No remaining volumes to retry.",
      });
    }
    if (attempt.environmentResourceId === null) {
      return yield* new Validation({
        field: "attemptId",
        message: "This volume remove has no Cloud volume to retry.",
      });
    }
    const resourceId = attempt.environmentResourceId;
    const created = yield*
      insertVolumeRemoveAttempt({
        organizationId: attempt.organizationId,
        requestedByUserId: actor.userId,
        environmentId: attempt.environmentId,
        environmentResourceId: resourceId,
        volumes: retry.volumes,
        retryOfAttemptId: attempt.id,
      });
    yield* dispatchVolumeRemoveRequested(created.id);
    return created;
  },
);

export const loadLatestVolumeRemoveAttempt = Effect.fn(
  "VolumeRemoval.loadLatest",
)(function* (actor: Actor, input: VolumeResourceInput) {
  yield* requireEnvironmentAccess(actor, input);
  return yield*
    loadLatestVolumeRemoveAttemptForResource({
      environmentId: input.environmentId,
      environmentResourceId: input.resourceId,
    });
});

export const loadVolumeRemoveAttemptActivity = Effect.fn("VolumeRemoval.load")(
  loadVolumeRemoveAttempt,
);

export const claimVolumeRemoveAttemptActivity = Effect.fn(
  "VolumeRemoval.claim",
)((input: Parameters<typeof claimVolumeRemoveAttempt>[0]) =>
  claimVolumeRemoveAttempt(input));

export const completeVolumeRemoveAttemptActivity = Effect.fn(
  "VolumeRemoval.complete",
)((input: Parameters<typeof completeVolumeRemoveAttempt>[0]) =>
  completeVolumeRemoveAttempt(input));

export const prepareVolumeRemoveAttemptActivity = Effect.fn(
  "VolumeRemoval.prepare",
)(function* (input: {
  readonly attemptId: string;
  readonly inngestRunId: string;
  readonly now: Date;
}) {
  const existing = yield* loadVolumeRemoveAttemptActivity(input.attemptId);
  if (existing === null) return { kind: "missing" as const };
  if (existing.status === "completed") {
    return { kind: "reconcile" as const, attempt: existing };
  }
  if (existing.status === "awaiting_deployment") {
    return { kind: "awaiting" as const, attempt: existing };
  }
  if (volumeRemoveIsTerminal(existing.status)) {
    return { kind: "terminal" as const, attempt: existing };
  }
  const claimed = yield* claimVolumeRemoveAttemptActivity(input);
  return { kind: "ready" as const, attempt: claimed.attempt };
});

export const removeVolumesActivity = Effect.fn("VolumeRemoval.removeVolumes")(
  function* (attempt: VolumeRemoveAttempt) {
    const runtime = yield* OrganizationRuntime;
    const session = yield* runtime.open(attempt.organizationId);
    if (session.status !== "connected") {
      return yield* new PloyzProviderError({
        operation: "open organization runtime",
        cause: session,
      });
    }
    return yield* session.connected
      .removeVolumes({ volumes: [...attempt.volumes], force: false })
      .pipe(
        Effect.map((raw) =>
          parseVolumeRemoveOutcome(raw, attempt.volumes),
        ),
      );
  },
);

function unknownVolumeRemoveFailure(cause: unknown) {
  const detail = `: ${describeFailureCause(cause)}`;
  return `Volume removal may have reached Ployz, but Cloud did not receive a terminal outcome${detail}`.slice(
    0,
    2_000,
  );
}

export const executeVolumeRemoveAttemptOnceActivity = Effect.fn(
  "VolumeRemoval.executeOnce",
)(function* (input: {
  readonly attemptId: string;
  readonly inngestRunId: string;
  readonly now: Date;
}) {
  const begun = yield* beginVolumeRemoveAttempt(input);
  if (begun.kind === "unknown") return begun;
  const outcomeExit = yield* Effect.exit(
    Effect.scoped(removeVolumesActivity(begun.attempt)),
  );
  if (Exit.isFailure(outcomeExit)) {
    const attempt = yield* markVolumeRemoveAttemptUnknown({
      attemptId: begun.attempt.id,
      inngestRunId: input.inngestRunId,
      failureMessage: unknownVolumeRemoveFailure(Cause.squash(outcomeExit.cause)),
      now: input.now,
    });
    return { kind: "unknown" as const, attempt };
  }
  const completion = volumeRemoveCompletion(begun.attempt, outcomeExit.value);
  const attempt = yield* completeVolumeRemoveAttempt({
    attemptId: begun.attempt.id,
    inngestRunId: input.inngestRunId,
    ...completion,
    now: input.now,
  });
  return { kind: "completed" as const, attempt };
});

export const reconcileVolumeRemoveTombstoneActivity = Effect.fn(
  "VolumeRemoval.reconcileTombstone",
)(function* (attempt: {
  readonly environmentId: string;
  readonly environmentResourceId: string | null;
}) {
  if (attempt.environmentResourceId === null) return;
  const resourceId = attempt.environmentResourceId;
  const database = yield* Database;
  yield* database.transaction(
    Effect.gen(function* () {
      const transaction = yield* Database;
      const rows = yield* transaction.drizzle
        .select({
          resourceId: schemaEnvironmentResource.id,
          environmentId: schemaEnvironmentResource.environmentId,
          implementationType: schemaEnvironmentResource.implementationType,
          isAuthored: volumeIsAuthored,
        })
        .from(schemaEnvironmentResource)
        .where(
          and(
            eq(schemaEnvironmentResource.id, resourceId),
            eq(schemaEnvironmentResource.environmentId, attempt.environmentId),
          ),
        )
        .limit(1);
      const authority = rows[0];
      if (
        authority === undefined ||
        authority.implementationType !== "volume" ||
        authority.isAuthored
      ) {
        return;
      }
      yield* transaction.drizzle
        .delete(schemaEnvironmentCanvasNodePosition)
        .where(
          and(
            eq(
              schemaEnvironmentCanvasNodePosition.environmentId,
              authority.environmentId,
            ),
            eq(schemaEnvironmentCanvasNodePosition.resourceType, "volume"),
            eq(
              schemaEnvironmentCanvasNodePosition.resourceId,
              authority.resourceId,
            ),
          ),
        );

    }),
  );
});

export const failOwnedVolumeRemoveAttemptActivity = Effect.fn(
  "fail-volume-remove-retry-exhausted",
)(function* (input: {
  readonly attemptId: string;
  readonly inngestRunId: string;
  readonly failureMessage: string;
  readonly now: Date;
}) {
  const attempt = yield* loadVolumeRemoveAttemptActivity(input.attemptId);
  if (
    attempt === null ||
    attempt.status !== "running" ||
    attempt.inngestRunId !== input.inngestRunId
  ) {
    return { state: "skipped" as const };
  }
  if (attempt.startedAt !== null) {
    yield* markVolumeRemoveAttemptUnknown({
      attemptId: attempt.id,
      inngestRunId: input.inngestRunId,
      failureMessage: unknownVolumeRemoveFailure(input.failureMessage),
      now: input.now,
    });
    return { state: "unknown" as const };
  }
  yield* completeVolumeRemoveAttemptActivity({
    attemptId: attempt.id,
    inngestRunId: input.inngestRunId,
    status: "failed",
    failureMessage: input.failureMessage,
    now: input.now,
  });
  return { state: "failed" as const };
});

export const cancelVolumeRemoveAttemptActivity = Effect.fn(
  "VolumeRemoval.cancelOwned",
)(function* (input: { readonly inngestRunId: string; readonly now: Date }) {
  const attempt = yield* loadVolumeRemoveAttemptByRun(input.inngestRunId);
  if (attempt === null || attempt.status !== "running") {
    return { state: "skipped" as const };
  }
  if (attempt.startedAt !== null) {
    yield* markVolumeRemoveAttemptUnknown({
      attemptId: attempt.id,
      inngestRunId: input.inngestRunId,
      failureMessage:
        "Volume removal may have reached Ployz before Cloud received the cancellation.",
      now: input.now,
    });
    return { state: "unknown" as const };
  }
  yield* completeVolumeRemoveAttemptActivity({
    attemptId: attempt.id,
    inngestRunId: input.inngestRunId,
    status: "cancelled",
    failureMessage: "Volume remove was cancelled.",
    now: input.now,
  });
  return { state: "cancelled" as const };
});

export function volumeRemoveCompletion(
  attempt: Pick<VolumeRemoveAttempt, "volumes">,
  outcome: VolumeRemoveOutcome,
) {
  return {
    status: volumeRemoveStatusFromOutcome(attempt.volumes, outcome),
    outcome,
  } as const;
}
