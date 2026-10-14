import "@tanstack/react-start/server-only";
import { and, asc, eq } from "drizzle-orm";
import { Effect } from "effect";
import { member, organizationToken, session } from "#/modules/identity/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createServerAccessRetireRequestedEvent } from "#/modules/inngest/events";
import { organization } from "#/modules/organization/tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

/**
 * The one place a membership ends. The user's Organization Tokens there go with it, their sessions acting there move to
 * their next Organization or none, and their devices' holders on its Servers are retired once it commits. It refuses
 * to remove the last owner. Locks the Organization's member rows, so a concurrent removal or role change waits.
 */
export const removeMembership = Effect.fn("Organization.removeMembership")(function* (organizationId: string, userId: string) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const members = yield* drizzle.select({ userId: member.userId, role: member.role }).from(member)
      .where(eq(member.organizationId, organizationId)).orderBy(asc(member.userId)).for("update");
    const leaving = members.find((row) => row.userId === userId);
    if (leaving === undefined) return yield* new NotFound({ message: "That user is not a member of this Organization." });
    if (leaving.role === "owner" && !members.some((row) => row.role === "owner" && row.userId !== userId)) {
      return yield* new Conflict({ message: "An Organization needs an owner. Make someone else an owner first.", userFacing: true });
    }
    yield* drizzle.delete(member).where(and(eq(member.organizationId, organizationId), eq(member.userId, userId)));
    yield* drizzle.delete(organizationToken)
      .where(and(eq(organizationToken.organizationId, organizationId), eq(organizationToken.userId, userId)));
    const [next] = yield* drizzle.select({ id: organization.id, slug: organization.slug }).from(member)
      .innerJoin(organization, eq(organization.id, member.organizationId))
      .where(eq(member.userId, userId)).orderBy(asc(member.createdAt)).limit(1);
    yield* drizzle.update(session)
      .set({ activeOrganizationId: next?.id ?? null, activeOrganizationSlug: next?.slug ?? null, updatedAt: new Date() })
      .where(and(eq(session.userId, userId), eq(session.activeOrganizationId, organizationId)));
    yield* afterDatabaseCommit(sendInngestEvent(createServerAccessRetireRequestedEvent()).pipe(
      Effect.catch((error) => Effect.logWarning("Server access retirement was not requested; the sweep does it.", error)),
    ));
  }));
});
