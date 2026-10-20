import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { and, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import { organizationMachine } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";

/**
 * Keeps each enrolled Server's name as its Cluster reports it, the last report winning, so Cloud can name a Server
 * the Cluster can't currently answer for. A Server the frame leaves out keeps the name Cloud has.
 */
export const recordMachineNames = Effect.fn("Machines.recordNames")(function* (
  organizationId: string,
  frame: Pick<RuntimeWatchView, "machines">,
) {
  const { drizzle } = yield* Database;
  for (const { machine } of frame.machines) {
    if (machine.name === "") continue;
    yield* drizzle.update(organizationMachine).set({ machineName: machine.name }).where(and(
      eq(organizationMachine.organizationId, organizationId),
      eq(organizationMachine.machineId, machine.id),
      sql`${organizationMachine.machineName} is distinct from ${machine.name}`,
    ));
  }
});
