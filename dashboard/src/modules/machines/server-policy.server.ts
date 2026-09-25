import "@tanstack/react-start/server-only";

import { Data, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { sendInngestEvent } from "#/modules/inngest/client";
import {
  createServerPolicyChangeRequestedEvent,
  type ServerPolicyChangeRequestedEventData,
} from "#/modules/inngest/events";
import {
  machineUpdateForPolicyChange,
  type RequestServerPolicyChangeInput,
} from "#/modules/machines/server-policy";
import { requireInfrastructureOrganization } from "#/modules/runtime/organization-access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Validation } from "#/server/public-error";

export class ServerPolicyProviderFailure extends Data.TaggedError(
  "ServerPolicyProviderFailure",
)<{ readonly operation: string; readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

/**
 * Queue one Server Policy change. Cloud keeps no desired-policy record; the
 * Servers page reads the result back from Runtime observation.
 */
export const requestServerPolicyChange = Effect.fn(
  "ServerPolicy.requestChange",
)(function* (actor: Actor, input: RequestServerPolicyChangeInput) {
  if (
    input.change.acceptsBuilds === undefined &&
    input.change.buildConcurrency === undefined
  ) {
    return yield* new Validation({
      message: "A Server Policy change must set at least one value.",
    });
  }
  const organization = yield* requireInfrastructureOrganization(
    actor,
    input.organizationSlug,
  );
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
