import "@tanstack/react-start/server-only";
import { and, asc, eq } from "drizzle-orm";
import { Effect } from "effect";
import { member, organizationToken, session, user } from "#/modules/identity/tables";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createServerAccessRetireRequestedEvent } from "#/modules/inngest/events";
import type { Role } from "#/modules/organization/access.server";
import { organization } from "#/modules/organization/tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

export const MANAGE = "Only an owner or admin of this Organization can do this.";
export const OWN = "Only an owner can make or remove an owner.";

const refusalFor = (members: ReadonlyArray<{ userId: string; role: Role }>, askedBy: string, touchesOwner: boolean) => {
  const role = members.find((row) => row.userId === askedBy)?.role;
  if (role !== "owner" && role !== "admin") return new Conflict({ message: MANAGE, userFacing: true });
  if (touchesOwner && role !== "owner") return new Conflict({ message: OWN, userFacing: true });
  return null;
};

/**
 * The one place a membership ends. The user's Organization Tokens there go with it, their sessions acting there move to
 * their next Organization or none, and their devices' holders on its Servers are retired once it commits. It refuses
 * to remove the last owner. Locks the Organization's member rows, so a concurrent removal or role change waits.
 */
export const removeMembership = Effect.fn("Organization.removeMembership")(function* (organizationId: string, userId: string, askedBy: string) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const members = yield* drizzle.select({ userId: member.userId, role: member.role }).from(member)
      .where(eq(member.organizationId, organizationId)).orderBy(asc(member.userId)).for("update");
    const leaving = members.find((row) => row.userId === userId);
    if (leaving === undefined) return yield* new NotFound({ message: "That user is not a member of this Organization." });
    const refusal = askedBy === userId ? null : refusalFor(members, askedBy, leaving.role === "owner");
    if (refusal !== null) return yield* refusal;
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

export const listMembers = Effect.fn("Organization.listMembers")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  return yield* drizzle.select({ userId: member.userId, name: user.name, email: user.email, role: member.role, joinedAt: member.createdAt })
    .from(member).innerJoin(user, eq(user.id, member.userId))
    .where(eq(member.organizationId, organizationId)).orderBy(asc(member.createdAt));
});

/** Gives a member a new role. Locks the member rows in the order `removeMembership` does. */
export const setMemberRole = Effect.fn("Organization.setMemberRole")(function* (
  organizationId: string,
  userId: string,
  role: Role,
  askedBy: string,
) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const members = yield* drizzle.select({ userId: member.userId, role: member.role }).from(member)
      .where(eq(member.organizationId, organizationId)).orderBy(asc(member.userId)).for("update");
    const target = members.find((row) => row.userId === userId);
    if (target === undefined) return yield* new NotFound({ message: "That user is not a member of this Organization." });
    const refusal = refusalFor(members, askedBy, target.role === "owner" || role === "owner");
    if (refusal !== null) return yield* refusal;
    if (target.role === "owner" && role !== "owner" && !members.some((row) => row.role === "owner" && row.userId !== userId)) {
      return yield* new Conflict({ message: "An Organization needs an owner. Make someone else an owner first.", userFacing: true });
    }
    yield* drizzle.update(member).set({ role }).where(and(eq(member.organizationId, organizationId), eq(member.userId, userId)));
  }));
});
