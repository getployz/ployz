import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { clusterDomain } from "./cluster-domain.collection";
import { organizationClusterDomain } from "./tables";
import { defineProjection } from "#/collections/change-log/projection.server";
import { Database } from "#/server/database.server";

// The token and certificate key stay on the server.
export const clusterDomainProjection = defineProjection(clusterDomain, (scoped) => Effect.gen(function* () {
  const database = yield* Database;
  const domain = organizationClusterDomain;
  return yield* database.drizzle.select({
    id: domain.organizationId, name: domain.name, recordsSyncedAt: domain.recordsSyncedAt, traffic: domain.traffic,
    certificateNotAfter: domain.certificateNotAfter, checkedAt: domain.checkedAt,
  }).from(domain).where(scoped(domain));
}));
