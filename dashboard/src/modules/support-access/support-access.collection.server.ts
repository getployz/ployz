import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { supportAccess } from "./support-access.collection";
import { organizationSupportAccess } from "./tables";
import { defineProjection } from "#/collections/change-log/projection.server";
import { Database } from "#/server/database.server";

export const supportAccessProjection = defineProjection(supportAccess, (scoped) => Effect.gen(function* () {
  const database = yield* Database;
  const support = organizationSupportAccess;
  return yield* database.drizzle.select({
    id: support.organizationId, allowed: support.allowed, variables: support.variables, configs: support.configs, logs: support.logs,
  }).from(support).where(scoped(support));
}));
