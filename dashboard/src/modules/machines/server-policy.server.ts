import "@tanstack/react-start/server-only";

import type { MachineId } from "@ployz/sdk";
import { Data, Effect } from "effect";
import type { StoreRefusal } from "#/lib/refusal";
import type { Actor, Caller } from "#/modules/identity/actor";
import { sendInngestEvent } from "#/modules/inngest/client";
import { type ServerPolicyChangeRequestedEventData, createKeyedServerPolicyChangeRequestedEvent, serverPolicyChangeRequested } from "#/modules/machines/machines.events";
import { drainActiveOn } from "#/modules/machines/server-drain.server";
import { targetedMessage, volumeRunTargeting } from "#/modules/machines/server-drain-volumes.server";
import {
  isEmptyPolicyChange,
  machineUpdateForPolicyChange,
  type RequestServerPolicyChangeInput,
  type ServerPolicyChange,
} from "#/modules/machines/server-policy";
import { enqueueEvent } from "#/modules/inngest/outbox.server";
import { requireAccess } from "#/modules/organization/access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Conflict, Validation } from "#/server/public-error";

export class ServerPolicyProviderFailure extends Data.TaggedError(
  "ServerPolicyProviderFailure",
)<{ readonly operation: string; readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

const DRAINING = "A drain is running on this server. Wait for it to finish.";

/**
 * A Drain turns services off for its Server and moves what runs there; turning them back on would undo it. The request
 * refuses so the user hears why at once. Applying a change that turns services on takes its Organization's Drain slot
 * (`DRAIN_SLOT`), so no Drain runs while it does; it checks again right before the update, which refuses one
 * requested while the change waited.
 */
const undoesDrain = (organizationId: string, machineId: MachineId, change: ServerPolicyChange) =>
  change.acceptsServices === true ? drainActiveOn(organizationId, machineId) : Effect.succeed(false);

/**
 * A Volume moving onto the Server starts its Service there once it lands, and that start doesn't ask whether the
 * Server takes services, so turning them off waits until no Volume Run targets it.
 */
const strandsMove = Effect.fn("ServerPolicy.strandsMove")(function* (organizationId: string, machineId: MachineId, change: ServerPolicyChange) {
  if (change.acceptsServices !== false) return null;
  const targeted = yield* volumeRunTargeting(organizationId, machineId);
  return targeted === null ? null : targetedMessage(targeted);
});

/** Why the change can't apply now: it would undo a Drain, or turn services off under a Volume moving there. */
const changeRefusal = Effect.fn("ServerPolicy.changeRefusal")(function* (
  organizationId: string, machineId: MachineId, change: ServerPolicyChange,
) {
  if (yield* undoesDrain(organizationId, machineId, change)) return DRAINING;
  return yield* strandsMove(organizationId, machineId, change);
});

const refuseWhileDraining = Effect.fn("ServerPolicy.refuseWhileDraining")(function* (
  organizationId: string, machineId: MachineId, change: ServerPolicyChange,
) {
  const refusal = yield* changeRefusal(organizationId, machineId, change);
  if (refusal !== null) return yield* new Conflict({ userFacing: true, message: refusal });
});

/**
 * Queue one Server Policy change. Cloud keeps no desired-policy record; the
 * Servers page reads the result back from Runtime observation.
 */
export const requestServerPolicyChange = Effect.fn(
  "ServerPolicy.requestChange",
)(function* (actor: Actor, input: RequestServerPolicyChangeInput) {
  if (isEmptyPolicyChange(input.change)) {
    return yield* new Validation({
      message: "A Server Policy change must set at least one value.",
    });
  }
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:operate");
  yield* refuseWhileDraining(organization.id, input.machineId, input.change);
  yield* sendInngestEvent(
    serverPolicyChangeRequested.create({ organizationId: organization.id, machineId: input.machineId, change: input.change }),
  );
});

/** Plans a keyed Server Policy change: admitted, it queues one event keyed by its attempt in the same commit. */
export const planKeyedServerPolicyChange = Effect.fn("ServerPolicy.planKeyed")(function* (
  caller: Caller, machineId: MachineId, change: ServerPolicyChange,
) {
  const refused = (code: string, message: string) =>
    ({ ok: false, refusal: { code, message, details: null } }) as const satisfies { ok: false; refusal: StoreRefusal };
  if (isEmptyPolicyChange(change)) return refused("invalid_argument", "A Server Policy change must set at least one value.");
  const { organization } = yield* requireAccess(caller, caller.organization.slug, "servers:operate");
  const refusal = yield* changeRefusal(organization.id, machineId, change);
  if (refusal !== null) return refused("conflict", refusal);
  return (attemptId: string) =>
    enqueueEvent(createKeyedServerPolicyChangeRequestedEvent({ organizationId: organization.id, machineId, change }, attemptId))
      .pipe(Effect.as({ ok: true, id: attemptId } as const));
});

export const applyServerPolicyChangeActivity = Effect.fn(
  "ServerPolicy.apply",
)(function* (request: ServerPolicyChangeRequestedEventData) {
  const runtime = yield* OrganizationRuntime;
  const session = yield* runtime.open(request.organizationId);
  if (session.status !== "connected") {
    return yield* new ServerPolicyProviderFailure({
      operation: "open organization runtime",
      cause: session,
    });
  }
  yield* refuseWhileDraining(request.organizationId, request.machineId, request.change);
  // Cloud machine ids are the Machine IDs Rust accepts as a Machine Target.
  yield* session.connected
    .updateMachine(request.machineId, machineUpdateForPolicyChange(request.change))
    .pipe(
      Effect.mapError(
        (cause) =>
          new ServerPolicyProviderFailure({ operation: "update machine", cause }),
      ),
    );
});
