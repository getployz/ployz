import { Schema } from "effect";
import { defineEvent } from "#/modules/inngest/define-event";

/** An Organization's Cluster may lack its Sealing Key: backfill it. Debounced per Organization, so a burst runs once. */
export const sealingKeyEnsureRequested = defineEvent({
  name: "sealing-key/ensure.requested",
  schema: Schema.Struct({ organizationId: Schema.NonEmptyString }),
});

/** Reseal the Legacy secrets of `organizationId`, or of every Organization that holds one. */
export const variablesResealRequested = defineEvent({
  name: "variables/reseal.requested",
  schema: Schema.Struct({ organizationId: Schema.optionalKey(Schema.String) }),
});

/** Sent when a worker starts, so an install whose Clusters reported their keys before it reseals too; resealing again changes nothing. */
export const createStartupResealEvent = (startedAt: Date) =>
  ({ id: `variables-reseal-start-${startedAt.getTime()}`, ...variablesResealRequested.create({}) }) as const;

/** Keyed by the Cluster whose first Sealing Key Cloud recorded: a retried record sends it again, and Inngest runs it once. */
export const createClusterResealRequestedEvent = ({ organizationId, clusterKey }: { organizationId: string; clusterKey: string }) =>
  ({ id: `variables-reseal-${organizationId}-${clusterKey}`, ...variablesResealRequested.create({ organizationId }) }) as const;
