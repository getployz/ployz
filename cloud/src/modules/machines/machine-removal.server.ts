import "@tanstack/react-start/server-only";

import type { MachineId } from "@ployz/sdk";
import { Data, Effect, Exit } from "effect";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createMachineRemoveRequestedEvent } from "#/modules/inngest/events";
import type { Actor } from "#/modules/identity/actor";
import {
  type DataLossList,
} from "#/modules/runtime/data-loss-confirm";
import type { PloyzSdkError } from "#/modules/runtime/ployz.server";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type {
  EnqueueMachineRemoveInput,
  GetMachineRemoveAttemptInput,
  LoadMachineDataLossInput,
  MachineRemoveAttemptContext,
  RemoveMachineOutcome,
} from "#/modules/machines/machine-removal";
import {
  abandonPendingMachineRemoveAttempt,
  claimMachineRemoveAttempt,
  completeMachineRemoveAttempt,
  loadAuthorizedMachineRemoveAttempt,
  loadMachineRemoveAttempt,
  loadMachineRemoveAttemptByRun,
  requestMachineRemoveAttempt,
} from "#/modules/machines/machine-removal.repository";
import { NotFound } from "#/server/public-error";

function asMachineId(machineId: string): MachineId {
  // SAFETY: Cloud machine ids are the same strings rust brands as MachineId.
  return machineId as MachineId;
}

export class MachineRemovalProviderFailure extends Data.TaggedError(
  "MachineRemovalProviderFailure",
)<{ readonly operation: string; readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

export function asRemoveMachineOutcome<R>(
  run: Effect.Effect<unknown, PloyzSdkError, R>,
) {
  return run.pipe(
    Effect.as({ kind: "removed" as const } satisfies RemoveMachineOutcome),
    Effect.catchTag("MissingDataLossIdentities", (cause) =>
      Effect.succeed({
        kind: "missing_identities" as const,
        identities: cause.identities,
      } satisfies RemoveMachineOutcome),
    ),
    Effect.mapError(
      (cause) =>
        new MachineRemovalProviderFailure({
          operation: "remove machine",
          cause,
        }),
    ),
  );
}

export const loadMachineRemoveAttemptActivity = Effect.fn(
  "MachineRemoval.loadAttempt",
)(loadMachineRemoveAttempt);

export const loadMachineRemoveAttemptByRunActivity = Effect.fn(
  "MachineRemoval.loadAttemptByRun",
)((inngestRunId: string) =>
  loadMachineRemoveAttemptByRun(inngestRunId));

export const claimMachineRemoveAttemptActivity = Effect.fn(
  "MachineRemoval.claim",
)((input: Parameters<typeof claimMachineRemoveAttempt>[0]) =>
  claimMachineRemoveAttempt(input));

export const completeMachineRemoveAttemptActivity = Effect.fn(
  "MachineRemoval.complete",
)((input: Parameters<typeof completeMachineRemoveAttempt>[0]) =>
  completeMachineRemoveAttempt(input));

export const removeMachineActivity = Effect.fn("MachineRemoval.remove")(
  function* (attempt: MachineRemoveAttemptContext) {
    const runtime = yield* OrganizationRuntime;
    const session = yield* runtime.open(attempt.organizationId);
    if (session.status !== "connected") {
      return yield* new MachineRemovalProviderFailure({
        operation: "open organization runtime",
        cause: session,
      });
    }
    return yield* asRemoveMachineOutcome(
      session.connected.removeMachine(asMachineId(attempt.machineId), {
        confirmed: [...attempt.confirmDataLoss],
      }),
    );
  },
);

function isTerminalMachineRemove(attempt: MachineRemoveAttemptContext) {
  return (
    attempt.state === "succeeded" ||
    attempt.state === "failed" ||
    attempt.state === "cancelled" ||
    attempt.state === "missing_identities"
  );
}

export const prepareMachineRemoveAttemptActivity = Effect.fn(
  "MachineRemoval.prepare",
)(function* (input: {
  readonly attemptId: string;
  readonly inngestRunId: string;
  readonly now: Date;
}) {
  const existing = yield* loadMachineRemoveAttemptActivity(input.attemptId);
  if (existing === null) {
    return { kind: "missing" as const, attemptId: input.attemptId };
  }
  if (isTerminalMachineRemove(existing)) {
    return { kind: "terminal" as const, attempt: existing };
  }
  const claimed = yield* claimMachineRemoveAttemptActivity(input);
  return { kind: "ready" as const, attempt: claimed.attempt };
});

export const failOwnedMachineRemoveAttemptActivity = Effect.fn(
  "fail-machine-remove",
)(function* (input: {
  readonly attemptId: string;
  readonly inngestRunId: string;
  readonly now: Date;
}) {
  const attempt = yield* loadMachineRemoveAttemptActivity(input.attemptId);
  if (
    attempt === null ||
    attempt.state !== "running" ||
    attempt.inngestRunId !== input.inngestRunId
  ) {
    return { state: "skipped" as const };
  }
  yield* completeMachineRemoveAttemptActivity({
    ...input,
    completion: {
      state: "failed",
      failureCode: "retry_exhausted",
      failureMessage: "Machine remove retries were exhausted.",
    },
  });
  return { state: "failed" as const };
});

export const cancelMachineRemoveAttemptActivity = Effect.fn(
  "MachineRemoval.cancelOwned",
)(function* (input: {
  readonly inngestRunId: string;
  readonly now: Date;
}) {
  const attempt = yield* loadMachineRemoveAttemptByRunActivity(
    input.inngestRunId,
  );
  if (attempt === null || attempt.state !== "running") {
    return { state: "skipped" as const };
  }
  yield* completeMachineRemoveAttemptActivity({
    attemptId: attempt.id,
    inngestRunId: input.inngestRunId,
    now: input.now,
    completion: {
      state: "cancelled",
      failureCode: "cancelled",
      failureMessage: "Machine remove was cancelled.",
    },
  });
  return { state: "cancelled" as const };
});

export const loadMachineDataLoss = Effect.fn("MachineRemoval.loadDataLoss")(
  function* (actor: Actor, input: LoadMachineDataLossInput) {
    const organization = yield* requireInfrastructureOrganization(
      actor,
      input.organizationSlug,
    );
    const runtime = yield* OrganizationRuntime;
    const session = yield* runtime.open(organization.id);
    if (session.status !== "connected") {
      return yield* new MachineRemovalProviderFailure({
        operation: "open organization runtime",
        cause: session,
      });
    }
    const rust = yield* session.connected
      .dataLossIfMachineRemoved(asMachineId(input.machineId))
      .pipe(
        Effect.map((observed) => observed.data_loss),
        Effect.mapError(
          (cause) =>
            new MachineRemovalProviderFailure({
              operation: "load machine data loss",
              cause,
            }),
        ),
      );
    return {
      rust,
      cloud: [{ kind: "machine", name: input.machineId }],
    } satisfies DataLossList;
  },
);

export const dispatchMachineRemoveRequested = Effect.fn(
  "MachineRemoval.dispatchRequested",
)(function* (attemptId: string) {
  yield* sendInngestEvent(createMachineRemoveRequestedEvent({ attemptId })).pipe(
    Effect.onExit((exit) =>
      Exit.isSuccess(exit)
        ? Effect.void
        : abandonPendingMachineRemoveAttempt(attemptId),
    ),
  );
});

export const enqueueMachineRemove = Effect.fn("MachineRemoval.enqueue")(
  function* (actor: Actor, input: EnqueueMachineRemoveInput) {
    const organization = yield* requireInfrastructureOrganization(
      actor,
      input.organizationSlug,
    );
    const requested = yield* requestMachineRemoveAttempt({
      organizationId: organization.id,
      requestedByUserId: actor.userId,
      machineId: input.machineId,
      confirmDataLoss: [...input.confirmDataLoss],
    });
    yield* dispatchMachineRemoveRequested(requested.id);
    return requested;
  },
);

export const getMachineRemoveAttempt = Effect.fn("MachineRemoval.getAttempt")(
  function* (actor: Actor, input: GetMachineRemoveAttemptInput) {
    const organization = yield* requireInfrastructureOrganization(
      actor,
      input.organizationSlug,
    );
    const attempt = yield*
      loadAuthorizedMachineRemoveAttempt({
        attemptId: input.attemptId,
        organizationId: organization.id,
      });
    if (attempt !== null) return attempt;
    return yield* new NotFound({
      message: "The machine removal attempt was not found.",
    });
  },
);
