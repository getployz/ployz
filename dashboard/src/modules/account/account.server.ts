import "@tanstack/react-start/server-only";
import { and, arrayContains, desc, eq, gt, isNull, notLike, or, sql } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { oauthClient, oauthConsent } from "#/modules/identity/oauth-tables";
import { CLI_USER_AGENT_PREFIX } from "#/modules/identity/organization-token.server";
import { grantStands, signOutSessionAgents, transactionWithAgentGrants } from "#/modules/identity/session-agents.server";
import { account, member, session } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";

/** better-auth stores an IPv6 address as its /64 written out in full. A mapped IPv4 keeps its dotted form. */
const shortIp = (ip: string) => ip.includes(":") && !ip.includes(".") ? URL.parse(`http://[${ip}]`)?.hostname.slice(1, -1) ?? ip : ip;

/** The user's live sessions, most recently active first; tokens never leave the server. */
export const listAccountSessions = Effect.fn("Account.listSessions")(function* (actor: Actor) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({ id: session.id, userAgent: session.userAgent, ipAddress: session.ipAddress, createdAt: session.createdAt, updatedAt: session.updatedAt })
    .from(session)
    .where(and(eq(session.userId, actor.userId), gt(session.expiresAt, new Date())))
    .orderBy(desc(session.updatedAt));
  return rows.map((row) => ({
    id: row.id,
    userAgent: row.userAgent ?? null,
    ipAddress: row.ipAddress === null ? null : shortIp(row.ipAddress),
    cli: row.userAgent?.startsWith(CLI_USER_AGENT_PREFIX) ?? false,
    createdAt: row.createdAt.toISOString(),
    lastActiveAt: row.updatedAt.toISOString(),
  }));
});

/** Sign-in methods linked to the user; secrets stay in the table. */
export const listLinkedAccounts = Effect.fn("Account.listLinked")(function* (actor: Actor) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({ id: account.id, providerId: account.providerId, accountId: account.accountId, createdAt: account.createdAt })
    .from(account)
    .where(eq(account.userId, actor.userId));
  return rows.map((row) => ({ ...row, createdAt: row.createdAt.toISOString() }));
});

/**
 * Ends one browser session of the user, disconnecting the agents connected from it as signing out there would. A CLI
 * session is skipped: `ployz logout` also retires its Server access, which deleting the row alone would not.
 */
export const revokeAccountSession = Effect.fn("Account.revokeSession")(function* (actor: Actor, sessionId: string) {
  yield* transactionWithAgentGrants(actor.userId, Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [ending] = yield* drizzle
      .select({ id: session.id })
      .from(session)
      .where(and(
        eq(session.id, sessionId),
        eq(session.userId, actor.userId),
        // SQL's NOT LIKE is never true for NULL, so a session with no user agent is allowed outright.
        or(isNull(session.userAgent), notLike(session.userAgent, `${CLI_USER_AGENT_PREFIX}%`)),
      ))
      .for("update");
    if (ending === undefined) return;
    yield* signOutSessionAgents(actor.userId, ending.id);
    yield* drizzle.delete(session).where(eq(session.id, ending.id));
    yield* signOutSessionAgents(actor.userId, ending.id);
  }));
});

/**
 * The agents the user allowed on /connect that can still act, newest first: the grant keeps the `ployz` scope, its app
 * isn't disabled, the membership predates it, and a browser that allowed it is still signed in, as `oauthCaller` checks.
 */
export const listConnectedAgents = Effect.fn("Account.listConnectedAgents")(function* (actor: Actor) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({ id: oauthConsent.id, name: oauthClient.name, organization: organization.name, allowedAt: oauthConsent.createdAt })
    .from(oauthConsent)
    .innerJoin(oauthClient, eq(oauthClient.clientId, oauthConsent.clientId))
    .innerJoin(organization, eq(sql`${organization.id}::text`, oauthConsent.referenceId))
    .innerJoin(member, and(eq(member.organizationId, organization.id), eq(member.userId, actor.userId)))
    .where(and(
      eq(oauthConsent.userId, actor.userId),
      arrayContains(oauthConsent.scopes, ["ployz"]),
      or(isNull(oauthClient.disabled), eq(oauthClient.disabled, false)),
      sql`date_trunc('second', ${member.createdAt}) <= ${oauthConsent.createdAt}`,
      grantStands,
    ))
    .orderBy(desc(oauthConsent.createdAt));
  return rows.map((row) => ({ ...row, name: row.name ?? null, allowedAt: row.allowedAt.toISOString() }));
});

/** Disconnects one of the user's agents by deleting its grant, as leaving the Organization does. Its tokens stop at once. */
export const disconnectAgent = Effect.fn("Account.disconnectAgent")(function* (actor: Actor, consentId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(oauthConsent).where(and(eq(oauthConsent.id, consentId), eq(oauthConsent.userId, actor.userId)));
});
