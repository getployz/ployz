import "@tanstack/react-start/server-only";
import { and, eq, gt } from "drizzle-orm";
import { Effect } from "effect";
import { member, organizationToken } from "#/modules/identity/tables";
import type { Caller } from "#/modules/identity/actor";
import { hashOrganizationTokenSecret, ORGANIZATION_TOKEN_PREFIX } from "#/modules/identity/organization-token.server";
import { organization } from "#/modules/organization/tables";
import { Auth } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { Forbidden, Unauthorized } from "#/server/public-error";

/**
 * The one place a `ployz` API request becomes a Caller. An Organization Token acts in its own Organization while
 * it is unexpired and its maker is still a member; a session acts in its active Organization while its user is a
 * member there. Anything else is refused.
 */
export const resolveCaller = Effect.fn("Identity.resolveCaller")(function* (headers: Headers) {
  const bearer = /^Bearer (\S+)$/.exec(headers.get("authorization") ?? "")?.[1];
  if (bearer?.startsWith(ORGANIZATION_TOKEN_PREFIX) === true) return yield* tokenCaller(bearer);
  const auth = yield* Auth;
  const session = yield* auth.getSession(headers);
  if (session === null) return yield* new Unauthorized();
  const organizationId = session.session.activeOrganizationId;
  const bound = organizationId == null
    ? undefined
    : yield* membership(session.user.id, organizationId);
  if (bound === undefined) {
    return yield* new Forbidden({ message: "The session's active Organization is not one of yours." });
  }
  return {
    userId: session.user.id,
    organization: bound,
    credential: { kind: "session", id: session.session.id },
  } satisfies Caller;
});

const tokenCaller = Effect.fn("Identity.tokenCaller")(function* (secret: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({
      id: organizationToken.id,
      userId: organizationToken.userId,
      organizationId: organization.id,
      slug: organization.slug,
    })
    .from(organizationToken)
    .innerJoin(organization, eq(organization.id, organizationToken.organizationId))
    .innerJoin(member, and(
      eq(member.organizationId, organizationToken.organizationId),
      eq(member.userId, organizationToken.userId),
    ))
    .where(and(
      eq(organizationToken.secretHash, hashOrganizationTokenSecret(secret)),
      gt(organizationToken.expiresAt, new Date()),
    ))
    .limit(1);
  const row = rows[0];
  // Unknown, revoked, expired, or its maker left the Organization: all read the same.
  if (row === undefined) return yield* new Unauthorized();
  return {
    userId: row.userId,
    organization: { id: row.organizationId, slug: row.slug },
    credential: { kind: "token", id: row.id },
  } satisfies Caller;
});

const membership = Effect.fn("Identity.membership")(function* (userId: string, organizationId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ id: organization.id, slug: organization.slug })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(and(eq(member.userId, userId), eq(member.organizationId, organizationId)))
    .limit(1);
  return rows[0];
});

/** The Organizations a Caller may act in: a session's memberships, or a token's one Organization. */
export const callerOrganizations = Effect.fn("Identity.callerOrganizations")(function* (caller: Caller) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ id: organization.id, slug: organization.slug, name: organization.name })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(caller.credential.kind === "token"
      ? and(eq(member.userId, caller.userId), eq(member.organizationId, caller.organization.id))
      : eq(member.userId, caller.userId))
    .orderBy(organization.slug);
  return rows.map((row) => ({ ...row, current: row.id === caller.organization.id }));
});
