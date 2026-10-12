import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { requireAccess } from "#/modules/organization/access.server";
import { SUPPORT_SHOWS_NONE, type SetSupportAccessInput, type SupportAccessRow } from "#/modules/support-access/support-access";
import { organizationSupportAccess } from "#/modules/support-access/tables";
import { Database } from "#/server/database.server";
import { Forbidden } from "#/server/public-error";

const rowColumns = {
  id: organizationSupportAccess.organizationId,
  allowed: organizationSupportAccess.allowed,
  variables: organizationSupportAccess.variables,
  configs: organizationSupportAccess.configs,
  logs: organizationSupportAccess.logs,
};

/**
 * One switch under Settings > Support, by an owner or admin. Support itself can't change what it may see. Turning
 * support access off turns every other switch off; turning one on while support access is off changes nothing.
 */
export const setSupportAccess = Effect.fn("SupportAccess.set")(function* (
  actor: Actor,
  { organizationSlug, change }: SetSupportAccessInput,
) {
  const access = yield* requireAccess(actor, organizationSlug, "manage");
  if (access.kind === "support") return yield* new Forbidden({ message: "Only an owner or admin of this Organization can do this." });
  const organizationId = access.organization.id;
  const { drizzle } = yield* Database;
  const by = { changedByUserId: actor.userId, updatedAt: new Date() };
  if (change.setting === "allowed") {
    const changed = change.on ? { allowed: true } : { allowed: false, ...SUPPORT_SHOWS_NONE };
    const [row] = yield* drizzle.insert(organizationSupportAccess).values({ organizationId, ...changed, ...by })
      .onConflictDoUpdate({ target: organizationSupportAccess.organizationId, set: { ...changed, ...by } })
      .returning(rowColumns);
    // ponytail: an upsert always returns its row; the fallback only satisfies the type.
    return row ?? ({ id: organizationId, allowed: change.on, ...SUPPORT_SHOWS_NONE } satisfies SupportAccessRow);
  }
  const [updated] = yield* drizzle.update(organizationSupportAccess).set({ [change.setting]: change.on, ...by })
    .where(and(eq(organizationSupportAccess.organizationId, organizationId), eq(organizationSupportAccess.allowed, true)))
    .returning(rowColumns);
  if (updated !== undefined) return updated;
  const [current] = yield* drizzle.select(rowColumns).from(organizationSupportAccess)
    .where(eq(organizationSupportAccess.organizationId, organizationId));
  return current ?? ({ id: organizationId, allowed: false, ...SUPPORT_SHOWS_NONE } satisfies SupportAccessRow);
});
