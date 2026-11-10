import "@tanstack/react-start/server-only";
import { and, desc, eq, sql } from "drizzle-orm";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import {
  type ClusterFeature,
  featuresOf,
  joinWords,
  listsEvery,
  missingFor,
} from "#/modules/cluster-requirements/cluster-requirements";
import { organizationCloudflareIngress } from "#/modules/ingress/tables";
import { firstRuntimeFrame } from "#/modules/runtime/organization-runtime.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { serverEnrollment } from "#/modules/machines/waiting/tables";
import { Database } from "#/server/database.server";
import { Conflict, Unavailable } from "#/server/public-error";

/**
 * Whether the Cluster's current founder was told to start it with every Door closed. Read from the instruction, not the
 * enrollment's end: the Machine founds as told before the enrollment's own run marks it joined, and one that failed
 * late may have founded all the same. A founder that ended before the current pairing began belongs to a Cluster since
 * replaced, and says nothing about this one.
 */
export const foundedClosed = Effect.fn("ClusterRequirements.foundedClosed")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [founder] = yield* drizzle.select({ closed: sql<boolean>`coalesce((${serverEnrollment.instruction}->>'ingressClosed')::boolean, false)` })
    .from(serverEnrollment)
    .leftJoin(organizationPairing, eq(organizationPairing.organizationId, serverEnrollment.organizationId))
    .where(and(
      eq(serverEnrollment.organizationId, organizationId),
      eq(serverEnrollment.role, "founder"),
      sql`(${organizationPairing.createdAt} is null or coalesce(${serverEnrollment.endedAt}, now()) >= ${organizationPairing.createdAt})`,
    ))
    .orderBy(desc(serverEnrollment.requestedAt))
    .limit(1);
  return founder?.closed ?? false;
});

/** Whether the Cloudflare setup is active and was last written within the current pairing, so a removed Cluster's row says nothing, and so does any row while no Cluster is paired. */
const tunnelOnly = Effect.fn("ClusterRequirements.tunnelOnly")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ active: sql<boolean>`${organizationCloudflareIngress.state} = 'active'` })
    .from(organizationCloudflareIngress)
    .innerJoin(organizationPairing, eq(organizationPairing.organizationId, organizationCloudflareIngress.organizationId))
    .where(and(
      eq(organizationCloudflareIngress.organizationId, organizationId),
      sql`${organizationCloudflareIngress.updatedAt} >= ${organizationPairing.createdAt}`,
    ));
  return row?.active ?? false;
});

/**
 * What Cloud recorded asking the Cluster for: a closed founding, or Cloudflare Tunnel active, both within the current
 * pairing. Read from the database alone, so it is safe under an Organization lock. A Cluster can turn a feature on without
 * Cloud, so this is the fallback evidence, and the Servers' own frame wins over it where it is seen whole.
 */
export const recordedFeatures = Effect.fn("ClusterRequirements.recorded")(function* (organizationId: string) {
  const closed = (yield* foundedClosed(organizationId)) || (yield* tunnelOnly(organizationId));
  return new Set<ClusterFeature>(closed ? ["closedDoors"] : []);
});

/**
 * What the Cluster requires, for the planner's early skip. With nothing recorded there is nothing to read; otherwise the
 * Servers are read once, outside any Organization lock, so a record that went stale (ports opened again) doesn't skip a
 * host that `enrollMachine` would admit. A frame that can't be read keeps the record.
 */
export const currentFeatures = Effect.fn("ClusterRequirements.current")(function* (organizationId: string) {
  const recorded = yield* recordedFeatures(organizationId);
  if (recorded.size === 0) return recorded;
  const frame = yield* firstRuntimeFrame(organizationId).pipe(Effect.catch(() => Effect.succeed(null)));
  return featuresOf({ recorded, frame });
});

/**
 * The gate for a joiner. Refuses one that lacks a capability the Cluster requires, before Cloud registers it. A joiner
 * that lists every Cluster Requirement reads nothing. Otherwise the Servers are read through `frame`, which fails to null.
 * A Cluster whose Servers can't be read fails closed for a joiner that lacks something: it may be closed without Cloud
 * knowing, so the join stops and the operator runs it again.
 */
export const refuseLackingJoiner = Effect.fn("ClusterRequirements.refuseJoiner")(function* (
  organizationId: string,
  advertised: readonly string[] | undefined,
  frame: Effect.Effect<RuntimeWatchView | null>,
) {
  if (listsEvery(advertised)) return;
  const recorded = yield* recordedFeatures(organizationId);
  const seen = yield* frame;
  const missing = missingFor(seen === null ? recorded : featuresOf({ recorded, frame: seen }), advertised);
  if (missing !== null) return yield* new Conflict({ message: joinWords(missing), userFacing: true });
  if (seen === null) return yield* new Unavailable({ message: "Cloud can't read which ports this Cluster keeps closed yet. Run this again in a minute.", userFacing: true });
});
