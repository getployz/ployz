import type { MachineUpgradeAttemptId } from "@ployz/sdk";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { defineEvent } from "#/modules/inngest/define-event";
import { UPGRADE_TRIGGERS } from "#/modules/server-upgrade/server-upgrade";

const AttemptIdType = Schema.declare<MachineUpgradeAttemptId>(
  (value): value is MachineUpgradeAttemptId => typeof value === "string",
);

/** A rollout request. A manual one names who clicked and the Server they clicked on. */
export const serverUpgradeRequested = defineEvent({
  name: "server/upgrade.requested",
  schema: Schema.Struct({
    organizationId: Schema.String.check(Schema.isNonEmpty()),
    /** The Server to Upgrade; null upgrades every Server behind. */
    machineId: Schema.NullOr(rustMachineIdSchema),
    trigger: Schema.Literals(UPGRADE_TRIGGERS),
    userId: Schema.NullOr(Schema.String),
    /** The attempt ID the CLI polls, minted by its request; only with `machineId`. Absent, the run mints one. */
    attemptId: Schema.optional(Schema.String.check(Schema.isPattern(/^[0-9a-f]{32}$/)).pipe(Schema.decodeTo(AttemptIdType))),
  }),
});

export const createCliServerUpgradeRequestedEvent = (data: typeof serverUpgradeRequested.schema.Type & { readonly attemptId: string }) =>
  ({ id: `server-upgrade-${data.attemptId}`, ...serverUpgradeRequested.create(data) }) as const;

export type ServerUpgradeRequestedEventData = typeof serverUpgradeRequested.schema.Type;
