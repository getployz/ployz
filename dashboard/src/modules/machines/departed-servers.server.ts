import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { departedServers } from "#/modules/machines/departed-servers";
import { dropRemovedServer } from "#/modules/machines/pairing-release.server";
import { machineRemoveAttempt, organizationMachine } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";

/**
 * Cloud learns a Server left from the Cluster, not from whoever removed it: a Server of pairing `generation` that a
 * frame no longer lists loses Cloud's row, as one the Cluster had already forgotten does (`unconfirmed`), so the last
 * Server keeps its row. "asked" while a departed Server still waits on its grace or on Cloud's own removal, so a
 * later frame reads it again.
 */
export const recordDepartedServers = Effect.fn("Machines.recordDeparted")(function* (
  organizationId: string,
  generation: string,
  frame: Pick<RuntimeWatchView, "machines" | "incomplete_ids">,
) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ machineId: organizationMachine.machineId, updatedAt: organizationMachine.updatedAt })
    .from(organizationMachine)
    .where(and(eq(organizationMachine.organizationId, organizationId), eq(organizationMachine.clusterKey, generation)));
  const removing = yield* drizzle.select({ machineId: machineRemoveAttempt.machineId }).from(machineRemoveAttempt)
    .where(and(eq(machineRemoveAttempt.organizationId, organizationId), inArray(machineRemoveAttempt.state, ["pending", "running"])));
  const { departed, held } = departedServers(rows, frame, new Date(), new Set(removing.map(({ machineId }) => machineId)));
  for (const machineId of departed) {
    const dropped = yield* dropRemovedServer({ organizationId, machineId, generation, removal: "unconfirmed" });
    yield* Effect.logInfo("The Cluster no longer lists this Server; Cloud let go of its row.", { organizationId, machineId, dropped: dropped.kind });
  }
  return held.length === 0 ? "settled" as const : "asked" as const;
});
