import "@tanstack/react-start/server-only";
import { and, arrayContains, eq, gt, isNull, or, sql } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import { oauthClient, oauthConsent } from "#/modules/identity/oauth-tables";
import { member, organizationToken, session } from "#/modules/identity/tables";
import type { Caller } from "#/modules/identity/actor";
import { hashOrganizationTokenSecret, ORGANIZATION_TOKEN_PREFIX } from "#/modules/identity/organization-token.server";
import { requireAccess } from "#/modules/organization/access.server";
import { organization } from "#/modules/organization/tables";
import { Auth } from "#/server/auth.server";
import { Database } from "#/server/database.server";
import { Forbidden, Unauthorized } from "#/server/public-error";

/**
 * The one place a `ployz` API request becomes a Caller. An Organization Token acts in its own Organization while
 * it is unexpired and its maker is still a member; a session acts in its active Organization while its user is a
 * member there. Anything else is refused. The session must come in the Authorization header, as the CLI sends it:
 * a cookie is what a browser attaches to a cross-site request on its own, so it authenticates nothing here.
 */
export const resolveCaller = Effect.fn("Identity.resolveCaller")(function* (headers: Headers) {
  const bearer = /^Bearer (\S+)$/.exec(headers.get("authorization") ?? "")?.[1];
  if (bearer?.startsWith(ORGANIZATION_TOKEN_PREFIX) === true) return yield* tokenCaller(bearer);
  const auth = yield* Auth;
  const presented = new Headers(headers);
  presented.delete("cookie");
  const session = yield* auth.getSession(presented, { bearer: true });
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
    organization: { id: bound.id, slug: bound.slug },
    credential: { kind: "session", id: session.session.id },
    role: bound.role,
    support: null,
    via: "cli",
  } satisfies Caller;
});

/** A dashboard request's signed-in user, acting in the Organization its URL names as a member or as support. */
export const dashboardCaller = Effect.fn("Identity.dashboardCaller")(function* (headers: Headers, organizationSlug: string) {
  const auth = yield* Auth;
  const session = yield* auth.getSession(headers);
  if (session === null) return yield* new Unauthorized();
  const access = yield* requireAccess({ userId: session.user.id }, organizationSlug, "operate");
  return {
    userId: session.user.id,
    organization: access.organization,
    credential: { kind: "session", id: session.session.id },
    role: access.kind === "member" ? access.role : null,
    support: access.kind === "support" ? access.shows : null,
    via: "dashboard",
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
      role: member.role,
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
    role: row.role,
    support: null,
    via: "cli",
  } satisfies Caller;
});

const GrantClaims = Schema.Struct({
  sub: Uuid,
  sid: Uuid,
  org: Uuid,
  grant: Uuid,
  azp: Schema.optionalKey(Schema.String),
  client_id: Schema.optionalKey(Schema.String),
});
export type GrantClaims = typeof GrantClaims.Type;

/** The Ployz grant a verified access token carries, or None when the token wasn't issued through /connect. */
export const grantClaimsOf = Schema.decodeUnknownOption(GrantClaims);

/**
 * A remote agent's verified access token, acting through the consent it was issued under. The consent names the
 * Organization; the grant ends when the consent is revoked or loses the `ployz` scope, the client is disabled, the signing-in session ends, or
 * the user leaves the Organization, which deletes the consent as disconnecting does. Rejoining doesn't revive it: the membership must
 * predate the consent's first Allow, which better-auth stamps in whole seconds and keeps when a later Allow updates the row.
 */
export const oauthCaller = Effect.fn("Identity.oauthCaller")(function* (claims: GrantClaims) {
  const { sub, sid, org, grant, azp, client_id } = claims;
  const clientId = azp ?? client_id;
  if (clientId === undefined) return yield* new Unauthorized();
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ slug: organization.slug, role: member.role })
    .from(oauthConsent)
    .innerJoin(oauthClient, eq(oauthClient.clientId, oauthConsent.clientId))
    .innerJoin(organization, eq(organization.id, org))
    .innerJoin(member, and(eq(member.organizationId, org), eq(member.userId, sub)))
    .innerJoin(session, and(eq(session.id, sid), eq(session.userId, sub)))
    .where(and(
      eq(oauthConsent.id, grant),
      eq(oauthConsent.userId, sub),
      eq(oauthConsent.clientId, clientId),
      eq(oauthConsent.referenceId, org),
      arrayContains(oauthConsent.scopes, ["ployz"]),
      or(isNull(oauthClient.disabled), eq(oauthClient.disabled, false)),
      gt(session.expiresAt, new Date()),
      sql`date_trunc('second', ${member.createdAt}) <= ${oauthConsent.createdAt}`,
    ))
    .limit(1);
  const row = rows[0];
  if (row === undefined) return yield* new Unauthorized();
  return {
    userId: sub,
    organization: { id: org, slug: row.slug },
    credential: { kind: "oauth", id: grant },
    role: row.role,
    support: null,
    via: "agent",
  } satisfies Caller;
});

const membership = Effect.fn("Identity.membership")(function* (userId: string, organizationId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ id: organization.id, slug: organization.slug, role: member.role })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(and(eq(member.userId, userId), eq(member.organizationId, organizationId)))
    .limit(1);
  return rows[0];
});

/** The Organizations a Caller may act in: a session's memberships, or a token's or grant's one Organization. */
export const callerOrganizations = Effect.fn("Identity.callerOrganizations")(function* (caller: Caller) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select({ id: organization.id, slug: organization.slug, name: organization.name })
    .from(member)
    .innerJoin(organization, eq(organization.id, member.organizationId))
    .where(caller.credential.kind === "session"
      ? eq(member.userId, caller.userId)
      : and(eq(member.userId, caller.userId), eq(member.organizationId, caller.organization.id)))
    .orderBy(organization.slug);
  return rows.map((row) => ({ ...row, current: row.id === caller.organization.id }));
});
