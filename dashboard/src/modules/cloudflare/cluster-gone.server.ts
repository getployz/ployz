import "@tanstack/react-start/server-only";
import { and, eq, gt, inArray, lte, ne, or } from "drizzle-orm";
import { Effect } from "effect";
import { SETUP_STALE_MS } from "#/modules/ingress/doors";
import { lockIngressRows } from "#/modules/ingress/doors.lock.server";
import { ingressDoorsAttempt, organizationCloudflareIngress } from "#/modules/ingress/tables";
import { Database } from "#/server/database.server";

/**
 * The Organization's Cluster is gone, so its Cloudflare setup and any switch of its Doors go with it: the next Cluster
 * starts as one that never set Cloudflare up, and can set it up or open its ports. The row goes whatever its state, so a
 * switch still running finds its attempt ended and writes nothing over a later row. The tunnel and wildcard record stay
 * the user's to delete in Cloudflare, and a new setup reuses them.
 *
 * One row stays: a `setting_up` reservation whose setup is still working in Cloudflare. Freeing it would let a new setup
 * take up the tunnel the old one is about to delete. It is marked instead, so that setup puts nothing back and drops the
 * row once its cleanup ends (a reservation past `SETUP_STALE_MS` belongs to a setup that died, and goes now).
 *
 * Called in the transaction that deletes the pairing, under the lock a setup and a switch take.
 */
export const endClusterIngress = Effect.fn("Cloudflare.endClusterIngress")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  yield* lockIngressRows(organizationId);
  const row = eq(organizationCloudflareIngress.organizationId, organizationId);
  const cutoff = new Date(Date.now() - SETUP_STALE_MS);
  const working = and(eq(organizationCloudflareIngress.state, "setting_up"), gt(organizationCloudflareIngress.updatedAt, cutoff));
  const ended = or(ne(organizationCloudflareIngress.state, "setting_up"), lte(organizationCloudflareIngress.updatedAt, cutoff));
  yield* drizzle.delete(organizationCloudflareIngress).where(and(row, ended));
  yield* drizzle.update(organizationCloudflareIngress).set({ clusterRemovedAt: new Date() }).where(and(row, working));
  yield* drizzle.update(ingressDoorsAttempt)
    .set({ state: "failed", failure: "The Cluster was removed.", endedAt: new Date() })
    .where(and(eq(ingressDoorsAttempt.organizationId, organizationId), inArray(ingressDoorsAttempt.state, ["pending", "running"])));
});
