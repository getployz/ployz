import "@tanstack/react-start/server-only";
import { and, desc, eq, inArray, sql } from "drizzle-orm";
import { Effect } from "effect";
import type { DoorsState } from "#/modules/ingress/doors";
import { ingressDoorsAttempt } from "#/modules/ingress/tables";
import { Database } from "#/server/database.server";

const ACTIVE_STATES: readonly DoorsState[] = ["pending", "running"];

/**
 * Serialize what writes the Organization's ingress rows: a switch request and Cloudflare setup's final write each take
 * this in their transaction, so setup can never land its `set_up` over a switch's state, nor a switch start on a
 * setup half written. Held until the transaction ends.
 */
export const lockIngressRows = Effect.fn("IngressDoors.lock")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`ingress-doors:${organizationId}`}))`);
});

/** The switch under way for the Organization, pending or running, or null. */
export const activeDoorsAttempt = Effect.fn("IngressDoors.active")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(ingressDoorsAttempt).where(and(
    eq(ingressDoorsAttempt.organizationId, organizationId), inArray(ingressDoorsAttempt.state, ACTIVE_STATES),
  )).orderBy(desc(ingressDoorsAttempt.requestedAt)).limit(1);
  return row ?? null;
});
