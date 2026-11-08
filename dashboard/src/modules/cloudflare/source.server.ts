import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { organizationCloudflareIngress } from "#/modules/ingress/tables";
import { Database } from "#/server/database.server";

/** The Organization's Cloudflare tunnel and zone, or null when it never set one up. */
export const loadCloudflareSetup = Effect.fn("Cloudflare.loadSetup")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({
    zone: organizationCloudflareIngress.zone,
    tunnelName: organizationCloudflareIngress.tunnelName,
    tunnelId: organizationCloudflareIngress.tunnelId,
    state: organizationCloudflareIngress.state,
  }).from(organizationCloudflareIngress).where(eq(organizationCloudflareIngress.organizationId, organizationId)).limit(1);
  return row ?? null;
});

/** The zone generated names sit under while the Cluster Domain's source is Cloudflare, else null. */
export const activeCloudflareZone = Effect.fn("Cloudflare.activeZone")(function* (organizationId: string) {
  const setup = yield* loadCloudflareSetup(organizationId);
  return setup?.state === "active" ? setup.zone : null;
});
