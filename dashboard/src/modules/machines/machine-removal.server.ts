import "@tanstack/react-start/server-only";

import type { LocalMachineRemoved, MachineId } from "@ployz/sdk";
import { Data, Effect, Exit } from "effect";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createMachineRemoveRequestedEvent } from "#/modules/inngest/events";
import type { Actor } from "#/modules/identity/actor";
import {
  type DataLossList,
} from "#/modules/runtime/data-loss-confirm";
import { type PloyzSdkError, rpcErrorCode, rpcErrorMessage } from "#/modules/runtime/ployz.server";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import type {
  EnqueueMachineRemoveInput,
  GetMachineRemoveAttemptInput,
  LoadMachineDataLossInput,
  MachineRemoveAttemptContext,
  RemoveMachineOutcome,
  ServerRelease,
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
import { dropRemovedServer, forgetEmptiedPairing } from "#/modules/machines/pairing-removal.server";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { storeSystem } from "#/modules/config-store/config-store.server";

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
  run: Effect.Effect<LocalMachineRemoved, PloyzSdkError, R>,
) {
  return run.pipe(
    Effect.map((removed): RemoveMachineOutcome => ({ kind: "removed", resetWarning: removed.reset_warning })),
    Effect.catchTag("MissingDataLossIdentities", (cause) =>
      Effect.succeed({
        kind: "missing_identities" as const,
        identities: cause.identities,
      } satisfies RemoveMachineOutcome),
    ),
    // A refusal changed nothing and says why; retrying it only delays the same answer.
    Effect.catchIf(
      (cause) => rpcErrorCode(cause) === "conflict",
      (cause) =>
        Effect.succeed({
          kind: "refused" as const,
          failureCode: "conflict",
          message: rpcErrorMessage(cause) ?? "The Server refused its removal.",
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

/**
 * Resets the Server under Cloud's own connection. The pairing generation Cloud reached it through comes back with the
 * outcome, as the witness `releaseServerActivity` checks before it lets go of anything.
 */
export const removeMachineActivity = Effect.fn("MachineRemoval.remove")(
  function* (attempt: Pick<MachineRemoveAttemptContext, "organizationId" | "machineId" | "confirmDataLoss" | "noReset">) {
    const access = yield* loadOrganizationConnections(attempt.organizationId);
    const runtime = yield* OrganizationRuntime;
    const session = yield* runtime.open(attempt.organizationId);
    if (access.kind !== "ready" || session.status !== "connected") {
      return yield* new MachineRemovalProviderFailure({
        operation: "open organization runtime",
        cause: session,
      });
    }
    const machine = asMachineId(attempt.machineId);
    const outcome = yield* asRemoveMachineOutcome(attempt.noReset
      ? session.connected.removeMachineMembership(machine).pipe(Effect.as({ reset_warning: null }))
      : session.connected.removeMachine(machine, { confirmed: [...attempt.confirmDataLoss] }));
    return { ...outcome, generation: access.generation };
  },
);

/**
 * What a removed Server leaves of Cloud's hold. A reset that didn't finish is a partial outcome: the Server may keep
 * Cloud's key, so Cloud keeps its row and its pairing. Otherwise `dropRemovedServer` drops the Server's row of the
 * witnessed pairing. When a reset took that pairing's last Server, the Store lets go of everything that ran, and then
 * Cloud forgets the pairing. A Server taken out without a reset loses its row but never lets go of the Cluster.
 */
export const releaseServerActivity = Effect.fn("MachineRemoval.release")(function* (input: {
  organizationId: string; machineId: string; generation: string; resetWarning: string | null; noReset: boolean;
}) {
  if (input.resetWarning !== null) {
    return {
      kind: "kept",
      reason: `its reset didn't finish (${input.resetWarning}), so it may still hold Cloud's key.`,
    } satisfies ServerRelease;
  }
  const dropped = yield* dropRemovedServer({ ...input, removal: input.noReset ? "membership" : "reset" });
  if (dropped.kind !== "last") return dropped;
  // The Store lets go first, while the pairing still fences a replacement: if this fails, nothing more is deleted and
  // the step retries; a replay finding the pairing gone skips it.
  yield* storeSystem(input.organizationId, { event: "cluster_forgotten" });
  return yield* forgetEmptiedPairing(input.organizationId, input.generation);
});

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
    return yield* startMachineRemove({
      organizationId: organization.id,
      requestedByUserId: actor.userId,
      machineId: input.machineId,
      confirmDataLoss: [...input.confirmDataLoss],
    });
  },
);

/** Start the one durable removal of a Server, the dashboard's and `ployz server rm`'s alike; both follow its attempt. */
export const startMachineRemove = Effect.fn("MachineRemoval.start")(
  function* (input: Parameters<typeof requestMachineRemoveAttempt>[0]) {
    const requested = yield* requestMachineRemoveAttempt(input);
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
