import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { type CloudflareSetup, SETUP_STALE_MS } from "#/modules/ingress/doors";
import { organizationCloudflareIngress } from "#/modules/ingress/tables";
import { Database } from "#/server/database.server";
import { tunnelTarget } from "./zone";

/**
 * The Organization's Cloudflare tunnel and zone, or null when it never set one up. A reservation a setup left behind
 * when it died (`setting_up` past `SETUP_STALE_MS`) reads as none, so the next setup takes it over.
 */
export const loadCloudflareSetup = Effect.fn("Cloudflare.loadSetup")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({
    zone: organizationCloudflareIngress.zone,
    tunnelName: organizationCloudflareIngress.tunnelName,
    tunnelId: organizationCloudflareIngress.tunnelId,
    state: organizationCloudflareIngress.state,
    updatedAt: organizationCloudflareIngress.updatedAt,
  }).from(organizationCloudflareIngress).where(eq(organizationCloudflareIngress.organizationId, organizationId)).limit(1);
  if (row === undefined) return null;
  const { zone, tunnelName, tunnelId, state } = row;
  if (state === "setting_up") {
    return row.updatedAt.getTime() < Date.now() - SETUP_STALE_MS ? null : { zone, tunnelName, tunnelId: null, state } satisfies CloudflareSetup;
  }
  // The check constraint keeps a tunnel on every state past `setting_up`; a row without one is a defect.
  if (tunnelId === null) return yield* Effect.die(new Error(`Cloudflare setup of ${organizationId} is ${state} without its tunnel.`));
  return { zone, tunnelName, tunnelId, state } satisfies CloudflareSetup;
});

/** The zone generated names sit under, and the CNAME target that reaches the tunnel, while the Cluster Domain's source is Cloudflare, else null. */
export const activeCloudflare = Effect.fn("Cloudflare.active")(function* (organizationId: string) {
  const setup = yield* loadCloudflareSetup(organizationId);
  return setup?.state === "active" ? { zone: setup.zone, target: tunnelTarget(setup.tunnelId) } : null;
});

/** The zone generated names sit under while the Cluster Domain's source is Cloudflare, else null. */
export const activeCloudflareZone = Effect.fn("Cloudflare.activeZone")(function* (organizationId: string) {
  return (yield* activeCloudflare(organizationId))?.zone ?? null;
});
