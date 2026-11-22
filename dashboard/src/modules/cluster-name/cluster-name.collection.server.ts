import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { clusterName } from "./cluster-name.collection";
import { organizationClusterName } from "./tables";
import { defineProjection } from "#/collections/change-log/projection.server";
import { Database } from "#/server/database.server";

export const clusterNameProjection = defineProjection(clusterName, (scoped) => Effect.gen(function* () {
  const database = yield* Database;
  const cluster = organizationClusterName;
  return yield* database.drizzle.select({
    id: cluster.organizationId, name: cluster.name, appliedName: cluster.appliedName, refusal: cluster.refusal,
  }).from(cluster).where(scoped(cluster));
}));
