import "@tanstack/react-start/server-only";
import { and, eq, exists, gt, isNull, sql } from "drizzle-orm";
import { Effect } from "effect";
import { oauthConsent, oauthRefreshToken } from "#/modules/identity/oauth-tables";
import { session } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";

/**
 * Signing a browser out disconnects the agents connected from it: each consent a refresh token was minted under in
 * that session is deleted, which ends the grant for `/mcp` and for every credential under it. Call it before the
 * session row goes, since deleting the row clears the tokens' session. A session that has already lapsed disconnects
 * nothing, so a connection outlives a browser that just stopped visiting.
 */
export const disconnectSessionAgents = Effect.fn("Identity.disconnectSessionAgents")(function* (sessionId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(oauthConsent).where(exists(
    drizzle.select({ id: oauthRefreshToken.id })
      .from(oauthRefreshToken)
      .innerJoin(session, eq(session.id, oauthRefreshToken.sessionId))
      .where(and(
        eq(session.id, sessionId),
        gt(session.expiresAt, new Date()),
        eq(oauthRefreshToken.clientId, oauthConsent.clientId),
        eq(oauthRefreshToken.userId, oauthConsent.userId),
        eq(oauthRefreshToken.referenceId, oauthConsent.referenceId),
        // A rotated token, or one from a consent since revoked, belongs to no grant standing now.
        isNull(oauthRefreshToken.revoked),
        sql`date_trunc('second', ${oauthRefreshToken.createdAt}) >= ${oauthConsent.createdAt}`,
      )),
  ));
});
