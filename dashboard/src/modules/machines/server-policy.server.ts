import "@tanstack/react-start/server-only";

import { Data, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { sendInngestEvent } from "#/modules/inngest/client";
import {
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
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Conflict, Validation } from "#/server/public-error";

export class ServerPolicyProviderFailure extends Data.TaggedError(
  "ServerPolicyProviderFailure",
)<{ readonly operation: string; readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

/**
 * A Drain turns services off for its Server and moves what runs there; turning them back on would undo it. The request
 * refuses so the user hears why at once. The apply shares its Organization's Drain slot, so no Drain runs while it
 * does; it checks again right before the update, which refuses one requested while the change waited.
 */
const refuseWhileDraining = Effect.fn("ServerPolicy.refuseWhileDraining")(function* (
  organizationId: string, machineId: string, change: ServerPolicyChange,
) {
  if (change.acceptsServices === true && (yield* drainActiveOn(organizationId, machineId))) {
    return yield* new Conflict({ userFacing: true, message: "A drain is running on this server. Wait for it to finish." });
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
  const organization = yield* requireInfrastructureOrganization(
    actor,
    input.organizationSlug,
  );
  yield* refuseWhileDraining(organization.id, input.machineId, input.change);
  yield* sendInngestEvent(
    createServerPolicyChangeRequestedEvent({
      organizationId: organization.id,
      machineId: input.machineId,
      change: input.change,
    }),
  );
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
