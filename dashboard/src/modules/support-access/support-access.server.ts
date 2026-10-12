import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import type { SetSupportAccessInput, SupportAccessRow } from "#/modules/support-access/support-access";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { Database } from "#/server/database.server";

/** "Allow support access" on Settings: an owner or admin, or support itself while it is allowed. */
export const setSupportAccess = Effect.fn("SupportAccess.set")(function* (
  actor: Actor,
  { organizationSlug, allowed }: SetSupportAccessInput,
) {
  const { organization } = yield* requireAccess(actor, organizationSlug, "manage");
  const { drizzle } = yield* Database;
  const changed = { allowed, changedByUserId: actor.userId };
  const [row] = yield* drizzle.insert(organizationSupportAccess).values({ organizationId: organization.id, ...changed })
    .onConflictDoUpdate({ target: organizationSupportAccess.organizationId, set: { ...changed, updatedAt: new Date() } })
    .returning({ id: organizationSupportAccess.organizationId, allowed: organizationSupportAccess.allowed });
  // ponytail: an upsert always returns its row; the fallback only satisfies the type.
  return row ?? ({ id: organization.id, allowed } satisfies SupportAccessRow);
});
