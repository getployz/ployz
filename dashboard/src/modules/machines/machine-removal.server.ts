import "@tanstack/react-start/server-only";

import type { LocalMachineRemoved, MachineId, MachineIdentity, MachineName, ObservedCopy } from "@ployz/sdk";
import { Data, Effect, Option } from "effect";
import { createMachineRemoveRequestedEvent } from "#/modules/inngest/events";
import type { Actor } from "#/modules/identity/actor";
import {
  type DataLossList,
} from "#/modules/runtime/data-loss-confirm";
import { type PloyzSdkError, rpcErrorCode, rpcErrorMessage, sdkFailureMessage } from "#/modules/runtime/ployz.server";
import { requireAccess } from "#/modules/organization/access.server";
import { OrganizationRuntime, RUNTIME_FRAME_TIMEOUT_MS } from "#/modules/runtime/organization-runtime.server";
import type {
  EnqueueMachineRemoveInput,
  GetMachineRemoveAttemptInput,
  LoadMachineDataLossInput,
  MachineRemoveAttemptContext,
  RemoveMachineOutcome,
  ServerRelease,
} from "#/modules/machines/machine-removal";
import { removalLack } from "#/modules/machines/machine-removal";
import {
  claimMachineRemoveAttempt,
  completeMachineRemoveAttempt,
  loadAuthorizedMachineRemoveAttempt,
  loadMachineRemoveAttempt,
  loadMachineRemoveAttemptByRun,
  markOwnedMachineRemoveSent,
  requestMachineRemoveAttempt,
} from "#/modules/machines/machine-removal.repository";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { dropRemovedServer, forgetEmptiedPairing } from "#/modules/machines/pairing-removal.server";
import { loadOrganizationConnections } from "#/modules/machines/connections.server";
import { planRemove } from "#/modules/machines/server-operations.server";
import { storeSystem } from "#/modules/config-store/config-store.server";
import { namedMachines, observeOrganizationCopies, releaseLanesOff } from "#/modules/volume-run/volume-run.server";

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
  servers: readonly { readonly id: string; readonly name: string }[] = [],
) {
  return run.pipe(
    Effect.map((removed): RemoveMachineOutcome => ({ kind: "removed", resetWarning: removed.reset_warning })),
    // The Leave's reply is the only proof the Server reset; the step's result keeps it for the Forget after it.
    Effect.catchTag("LeftNotForgotten", (left) =>
      Effect.succeed({ kind: "left" as const, resetWarning: left.resetWarning } satisfies RemoveMachineOutcome),
    ),
    Effect.catchTag("ServerLacks", (lack) =>
      Effect.succeed({
        kind: "refused" as const,
        failureCode: "server_lacks",
        message: sdkFailureMessage(lack, servers),
      } satisfies RemoveMachineOutcome),
    ),
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
 * Resets the Server under Cloud's own connection. A step retry sends the same removal again, and the SDK runs every
 * guard again while the Server is listed; one already gone by its Machine ID answers removed, its reset unconfirmed,
 * so Cloud keeps its row with that reason. A Server that answered its Leave but whose row the entry couldn't Forget
 * answers left; given that reply as `left`, this sends Forget alone and answers removed with the Leave's warning.
 * A run whose attempt is no longer its running attempt asks nothing and answers superseded. The pairing generation
 * Cloud reached it through comes back with the outcome, as the witness `releaseServerActivity` checks before it lets
 * go of anything.
 */
export const removeMachineActivity = Effect.fn("MachineRemoval.remove")(
  function* (attempt: Pick<MachineRemoveAttemptContext, "id" | "organizationId" | "machineId" | "confirmDataLoss" | "noReset"> & {
    readonly inngestRunId: string;
  }, left: { readonly resetWarning: string | null } | null = null) {
    const forgetOnly = attempt.noReset || left !== null;
    const access = yield* loadOrganizationConnections(attempt.organizationId);
    const runtime = yield* OrganizationRuntime;
    const session = yield* runtime.open(attempt.organizationId);
    if (access.kind !== "ready" || session.status !== "connected") {
      return yield* new MachineRemovalProviderFailure({
        operation: "open organization runtime",
        cause: session,
      });
    }
    // Without a frame every Server's capabilities are unknown, and an unknown capability attempts.
    const frame = yield* Effect.option(session.connected.watchFirstFrame(RUNTIME_FRAME_TIMEOUT_MS));
    const servers = Option.match(frame, {
      onNone: () => [],
      onSome: ({ machines }) => machines.map(({ machine }) => ({
        id: machine.id, name: machine.name, capabilities: machine.runtime.capabilities ?? [],
      })),
    });
    const lack = removalLack(servers, {
      target: attempt.machineId,
      entries: access.connections.flatMap((connection) => connection.machine_id ?? []),
      noReset: forgetOnly,
    });
    if (lack !== null) {
      return { kind: "refused", failureCode: "server_lacks", message: sdkFailureMessage(lack, servers), generation: access.generation } as const;
    }
    const machine = asMachineId(attempt.machineId);
    const owned = yield* markOwnedMachineRemoveSent({ attemptId: attempt.id, inngestRunId: attempt.inngestRunId });
    if (!owned) return { kind: "superseded" } as const;
    const outcome = yield* asRemoveMachineOutcome(forgetOnly
      ? session.connected.removeMachineMembership(machine).pipe(Effect.as({ reset_warning: left?.resetWarning ?? null }))
      : session.connected.removeMachine(machine, { confirmed: [...attempt.confirmDataLoss] }), servers);
    return { ...outcome, generation: access.generation };
  },
);

export const observeLeavingServerActivity = Effect.fn("MachineRemoval.observeLeaving")(function* (input: {
  organizationId: string; machineId: string;
}) {
  const id = asMachineId(input.machineId);
  const servers = yield* namedMachines(input.organizationId);
  const machine = { id, name: servers.find((server) => server.id === id)?.name ?? null };
  const copies = yield* observeOrganizationCopies(input.organizationId).pipe(Effect.orElseSucceed(() => []));
  const writes = copies.flatMap((copy) => copy.machine_id === id && copy.role === "writer" ? [copy.name] : []);
  return { machine, writes, held: heldOff(copies, servers, id) };
});

/**
 * A copy on another Server, by Docker Volume: what a lane committed to the leaving Server names to restore from. A
 * writer counts: a lane commits only after its source's cut, so a writer seen before the commit is a slot by the time
 * the lane ends. A copy's snapshots are not observed here, so a slot may still lack one.
 */
export function heldOff(copies: readonly ObservedCopy[], servers: readonly MachineIdentity[], leaving: MachineId) {
  const held: Record<string, MachineName> = {};
  for (const copy of copies) {
    const holder = copy.machine_id === leaving ? undefined : servers.find((server) => server.id === copy.machine_id);
    if (holder !== undefined) held[copy.name] ??= holder.name;
  }
  return held;
}

export const releaseLeavingLanesActivity = (input: {
  organizationId: string; machineId: string; name: string | null; writes: readonly string[]; held?: Readonly<Record<string, MachineName>>;
}) => releaseLanesOff(input.organizationId, { id: asMachineId(input.machineId), name: input.name }, input.writes, input.held);

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
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
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

export const enqueueMachineRemove = Effect.fn("MachineRemoval.enqueue")(
  function* (actor: Actor, input: EnqueueMachineRemoveInput) {
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
    // The same frame check an agent's removal runs: a Server Cloud doesn't see is never sent a removal.
    const plan = yield* planRemove(organization.id, input.machineId, input.confirmDataLoss);
    if (plan === null) {
      return yield* new NotFound({ message: "The Server was not found." });
    }
    return yield* startMachineRemove({
      organizationId: organization.id,
      requestedByUserId: actor.userId,
      machineId: input.machineId,
      confirmDataLoss: [...input.confirmDataLoss],
    });
  },
);

/**
 * Start the one durable removal of a Server, the dashboard's and `ployz server rm`'s alike; both follow its attempt.
 * The attempt and its run's event commit together.
 */
export const startMachineRemove = Effect.fn("MachineRemoval.start")(
  function* (input: Parameters<typeof requestMachineRemoveAttempt>[0]) {
    const { transaction } = yield* Database;
    return yield* transaction(Effect.gen(function* () {
      const requested = yield* requestMachineRemoveAttempt(input);
      yield* enqueueEvent(createMachineRemoveRequestedEvent({ attemptId: requested.id }));
      return requested;
    }));
  },
);

export const getMachineRemoveAttempt = Effect.fn("MachineRemoval.getAttempt")(
  function* (actor: Actor, input: GetMachineRemoveAttemptInput) {
    const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
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
