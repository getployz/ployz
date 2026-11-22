import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { defineEvent } from "#/modules/inngest/define-event";
import { ServerPolicyChangeSchema } from "#/modules/machines/server-policy";

const AttemptId = Schema.String.check(Schema.isUUID());
const OrganizationId = Schema.String.check(Schema.isNonEmpty());

export const machineRemoveRequested = defineEvent({
  name: "machine/remove.requested",
  schema: Schema.Struct({ attemptId: Schema.NonEmptyString }),
  id: ({ attemptId }) => `machine-remove-${attemptId}`,
});

/** No row owns this event: the Servers page reads the result back from Runtime observation. */
export const serverPolicyChangeRequested = defineEvent({
  name: "machine/policy-change.requested",
  schema: Schema.Struct({ organizationId: Schema.NonEmptyString, machineId: rustMachineIdSchema, change: ServerPolicyChangeSchema }),
});

/** Keyed by the attempt its request key admits, so a resend runs nothing new. */
export const createKeyedServerPolicyChangeRequestedEvent = (data: typeof serverPolicyChangeRequested.schema.Type, attemptId: string) =>
  ({ id: `server-policy-${attemptId}`, ...serverPolicyChangeRequested.create(data) }) as const;

/** A Drain was requested: its row is written. `organizationId` keys the Organization's one-at-a-time Drains. */
export const serverDrainRequested = defineEvent({
  name: "server/drain.requested",
  schema: Schema.Struct({ attemptId: AttemptId, organizationId: OrganizationId, machineId: rustMachineIdSchema }),
  id: ({ attemptId }) => `server-drain-${attemptId}`,
});

/** A web traffic change was requested: its row is written. `organizationId` keys the Organization's one-at-a-time changes. */
export const serverWebTrafficRequested = defineEvent({
  name: "server/web-traffic.requested",
  schema: Schema.Struct({ attemptId: AttemptId, organizationId: OrganizationId, machineId: rustMachineIdSchema }),
  id: ({ attemptId }) => `server-web-traffic-${attemptId}`,
});

/** A clean of a Namespace no Environment owns was requested: its row is written. */
export const namespaceCleanupRequested = defineEvent({
  name: "namespace/cleanup.requested",
  schema: Schema.Struct({ cleanupId: AttemptId, organizationId: OrganizationId }),
  id: ({ cleanupId }) => `namespace-cleanup-${cleanupId}`,
});

/** A member left an Organization (when the request named it): their devices' holders on its Servers go now. */
export const serverAccessRetireRequested = defineEvent({
  name: "server-access/retire.requested",
  schema: Schema.Struct({}),
});

export type ServerPolicyChangeRequestedEventData = typeof serverPolicyChangeRequested.schema.Type;
export type ServerDrainRequestedEventData = typeof serverDrainRequested.schema.Type;
export type ServerWebTrafficRequestedEventData = typeof serverWebTrafficRequested.schema.Type;
export type NamespaceCleanupRequestedEventData = typeof namespaceCleanupRequested.schema.Type;
