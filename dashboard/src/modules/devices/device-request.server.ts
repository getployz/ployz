import "@tanstack/react-start/server-only";
import { and, eq, gt, sql } from "drizzle-orm";
import { Effect } from "effect";
import { deviceCodeOf } from "#/modules/devices/device-code";
import { deviceRequest } from "#/modules/devices/tables";
import { Database } from "#/server/database.server";

/**
 * Holds `key`'s request in the Organization for a day, under the code Cloud computes from it. The same key again
 * refreshes it, and an expired request at its code gives way; null when another key's request holds the code.
 */
export const requestDevice = Effect.fn("Devices.requestDevice")(function* (organizationId: string, key: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.insert(deviceRequest)
    .values({ organizationId, code: deviceCodeOf(key), key, expiresAt: sql`now() + interval '24 hours'` })
    .onConflictDoUpdate({
      target: [deviceRequest.organizationId, deviceRequest.code],
      set: {
        key: sql`excluded.key`,
        expiresAt: sql`excluded.expires_at`,
        createdAt: sql`case when ${deviceRequest.key} = excluded.key then ${deviceRequest.createdAt} else excluded.created_at end`,
      },
      setWhere: sql`${deviceRequest.key} = excluded.key or ${deviceRequest.expiresAt} <= now()`,
    })
    .returning({ code: deviceRequest.code, expiresAt: deviceRequest.expiresAt });
  return row ?? null;
});

/** The Organization's unexpired request at `code`, or null. Reading leaves it for the next read. */
export const readDeviceRequest = Effect.fn("Devices.readDeviceRequest")(function* (organizationId: string, code: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ code: deviceRequest.code, key: deviceRequest.key, expiresAt: deviceRequest.expiresAt })
    .from(deviceRequest)
    .where(and(eq(deviceRequest.organizationId, organizationId), eq(deviceRequest.code, code), gt(deviceRequest.expiresAt, sql`now()`)))
    .limit(1);
  return row ?? null;
});
