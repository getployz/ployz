import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { member as schemaMember } from "#/modules/identity/tables";
import { organization as schemaOrganization } from "#/modules/organization/tables";
import type { Actor } from "#/modules/identity/actor";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

export const requireInfrastructureOrganization = Effect.fn(
  "InfrastructureOrganization.require",
)(function* (actor: Actor, organizationSlug: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      id: schemaOrganization.id,
      slug: schemaOrganization.slug,
    })
    .from(schemaMember)
    .innerJoin(
      schemaOrganization,
      eq(schemaMember.organizationId, schemaOrganization.id),
    )
    .where(
      and(
        eq(schemaMember.userId, actor.userId),
        eq(schemaOrganization.slug, organizationSlug),
      ),
    )
    .limit(1);
  const organization = rows[0];
  if (organization !== undefined) return organization;
  return yield* new NotFound({
    message: "The organization was not found.",
  });
});
