import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { lockIngressRows } from "#/modules/ingress/doors.lock.server";
import { organizationCloudflareIngress } from "#/modules/ingress/tables";
import { Database } from "#/server/database.server";

/**
 * The Organization's Cluster is gone, so nothing runs its tunnel. A reservation (`setting_up`) goes, and the setup that
 * made it finds it taken. A tunnel that was set up or in use becomes `left`: the Cluster Domain's source is Hosted again,
 * the next Cluster can set Cloudflare up fresh, and the tunnel and wildcard record stay the user's to delete. Called in
 * the transaction that deletes the pairing, under the lock a setup and a switch take, so none lands after it.
 */
export const endCloudflareSetup = Effect.fn("Cloudflare.endSetup")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  yield* lockIngressRows(organizationId);
  const row = eq(organizationCloudflareIngress.organizationId, organizationId);
  yield* drizzle.delete(organizationCloudflareIngress).where(and(row, eq(organizationCloudflareIngress.state, "setting_up")));
  yield* drizzle.update(organizationCloudflareIngress).set({ state: "left", updatedAt: new Date() })
    .where(and(row, inArray(organizationCloudflareIngress.state, ["set_up", "active"])));
});
