import "@tanstack/react-start/server-only";

import { Data, Effect } from "effect";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import type { Actor, Caller } from "#/modules/identity/actor";
import { sendInngestEvent } from "#/modules/inngest/client";
import {
  createKeyedServerPolicyChangeRequestedEvent,
  createServerPolicyChangeRequestedEvent,
  type ServerPolicyChangeRequestedEventData,
} from "#/modules/inngest/events";
import { drainActiveOn } from "#/modules/machines/server-drain.server";
import {
  isEmptyPolicyChange,
  machineUpdateForPolicyChange,
  type RequestServerPolicyChangeInput,
  type ServerPolicyChange,
} from "#/modules/machines/server-policy";
import { enqueueEvent } from "#/modules/operations/outbox.server";
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
const undoesDrain = (organizationId: string, machineId: string, change: ServerPolicyChange) =>
  change.acceptsServices === true ? drainActiveOn(organizationId, machineId) : Effect.succeed(false);

const refuseWhileDraining = Effect.fn("ServerPolicy.refuseWhileDraining")(function* (
  organizationId: string, machineId: string, change: ServerPolicyChange,
) {
  if (yield* undoesDrain(organizationId, machineId, change)) {
    return yield* new Conflict({ userFacing: true, message: DRAINING });
  }
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
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  yield* refuseWhileDraining(organization.id, input.machineId, input.change);
  yield* sendInngestEvent(
    createServerPolicyChangeRequestedEvent({ organizationId: organization.id, machineId: input.machineId, change: input.change }),
  );
});

/** Plans a keyed Server Policy change: admitted, it queues one event keyed by its attempt in the same commit. */
export const planKeyedServerPolicyChange = Effect.fn("ServerPolicy.planKeyed")(function* (
  caller: Caller, machineId: string, change: ServerPolicyChange,
) {
  const refused = (code: string, message: string) =>
    ({ ok: false, refusal: { code, message, details: null } }) as const satisfies { ok: false; refusal: StoreRefusal };
  if (isEmptyPolicyChange(change)) return refused("invalid_argument", "A Server Policy change must set at least one value.");
  const { organization } = yield* requireAccess(caller, caller.organization.slug, "operate");
  if (yield* undoesDrain(organization.id, machineId, change)) return refused("conflict", DRAINING);
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
