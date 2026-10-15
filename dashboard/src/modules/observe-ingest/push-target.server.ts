import "@tanstack/react-start/server-only";
import { and, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import type { MachineId } from "#/db/tables";
import { decryptPairingSecret } from "#/modules/machines/connections.server";
import { organizationMachine } from "#/modules/machines/tables";
import { ServerLacks } from "#/modules/runtime/daemon-compat";
import { Ployz } from "#/modules/runtime/ployz.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

const SERVER_TIMEOUT_MS = 10_000;
const SERVER_CONCURRENCY = 4;

export const observePushUrl = (origin: URL, machineId: MachineId) => new URL(`/api/machines/${machineId}/observe`, origin).href;

/**
 * Point every Machine whose confirmed push target isn't this Cloud's observe URL at it. A publish clears the
 * confirmation, so a new capability is re-pointed; a Machine that predates push targets stays pending.
 */
export const deliverPushTargets = Effect.fn("ObservePushTarget.deliver")(function* () {
  const { drizzle } = yield* Database;
  const config = yield* AppConfig;
  const rows = yield* drizzle.select({
    organizationId: organizationMachine.organizationId,
    machineId: organizationMachine.machineId,
    capability: organizationMachine.encryptedCapability,
    pushTarget: organizationMachine.pushTarget,
  }).from(organizationMachine);
  const pending = rows.flatMap((row) => {
    const url = observePushUrl(config.app.url, row.machineId);
    return row.pushTarget === url ? [] : [{ ...row, url }];
  });
  const outcomes = yield* Effect.forEach(pending, (row) => Effect.gen(function* () {
    const management = yield* decryptPairingSecret(row.capability);
    yield* Effect.scoped(Effect.gen(function* () {
      const ployz = yield* Ployz;
      const client = yield* ployz.connect({ connections: [{ machine_id: row.machineId, management }], timeoutMs: SERVER_TIMEOUT_MS });
      yield* client.setPushTarget(row.url);
    })).pipe(Effect.timeout(SERVER_TIMEOUT_MS * 2));
    // A publish that lands mid-dial carries a new capability; leave its confirmation for the next sweep.
    yield* drizzle.update(organizationMachine).set({ pushTarget: row.url }).where(and(
      eq(organizationMachine.organizationId, row.organizationId),
      eq(organizationMachine.machineId, row.machineId),
      eq(sql`${organizationMachine.encryptedCapability}->>'iv'`, row.capability.iv),
    ));
    return "set" as const;
  }).pipe(
    Effect.catch((error) => Effect.gen(function* () {
      if (error instanceof ServerLacks) {
        yield* Effect.logInfo("Server predates push targets; it keeps crash events until it upgrades.", { machineId: row.machineId });
        return "lacks" as const;
      }
      yield* Effect.logWarning("Push target not set; the next sweep retries.", { machineId: row.machineId, error });
      return "unreachable" as const;
    })),
    Effect.map((delivery) => ({ machineId: row.machineId, delivery })),
  ), { concurrency: SERVER_CONCURRENCY });
  return outcomes;
});
