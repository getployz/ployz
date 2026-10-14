import "@tanstack/react-start/server-only";
import { and, desc, eq, gt, notLike } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { CLI_USER_AGENT_PREFIX } from "#/modules/identity/organization-token.server";
import { account, session } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";

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
    ipAddress: row.ipAddress ?? null,
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
 * Ends one browser session of the user. A CLI session is skipped: `ployz logout` also retires its Server access,
 * which deleting the row alone would not.
 */
export const revokeAccountSession = Effect.fn("Account.revokeSession")(function* (actor: Actor, sessionId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle
    .delete(session)
    .where(and(eq(session.id, sessionId), eq(session.userId, actor.userId), notLike(session.userAgent, `${CLI_USER_AGENT_PREFIX}%`)));
});
