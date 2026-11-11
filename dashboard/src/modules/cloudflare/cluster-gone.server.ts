import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { lockIngressRows } from "#/modules/ingress/doors.lock.server";
import { ingressDoorsAttempt, organizationCloudflareIngress } from "#/modules/ingress/tables";
import { Database } from "#/server/database.server";

/**
 * The Organization's Cluster is gone, so its Cloudflare setup and any switch of its Doors go with it: the next Cluster
 * starts as one that never set Cloudflare up, and can set it up or open its ports. The row goes whatever its state, so a
 * setup or switch still running finds nothing to write over. The tunnel and wildcard record stay the user's to delete in
 * Cloudflare, and a new setup reuses them. Called in the transaction that deletes the pairing, under the lock a setup and
 * a switch take.
 */
export const endClusterIngress = Effect.fn("Cloudflare.endClusterIngress")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  yield* lockIngressRows(organizationId);
  yield* drizzle.delete(organizationCloudflareIngress).where(eq(organizationCloudflareIngress.organizationId, organizationId));
  yield* drizzle.update(ingressDoorsAttempt)
    .set({ state: "failed", failure: "The Cluster was removed.", endedAt: new Date() })
    .where(and(eq(ingressDoorsAttempt.organizationId, organizationId), inArray(ingressDoorsAttempt.state, ["pending", "running"])));
});
