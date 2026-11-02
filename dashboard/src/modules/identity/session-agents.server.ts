import "@tanstack/react-start/server-only";
import { and, eq, exists, gt, notExists } from "drizzle-orm";
import { Effect } from "effect";
import { oauthConsent, oauthConsentSession } from "#/modules/identity/oauth-tables";
import { session } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";

/** Records that `sessionId` allowed the consent standing for this client, user and Organization on /connect. */
export const linkConsentSession = Effect.fn("Identity.linkConsentSession")(function* (
  grant: { readonly clientId: string; readonly userId: string; readonly organizationId: string },
  sessionId: string,
) {
  const { drizzle } = yield* Database;
  const [consent] = yield* drizzle.select({ id: oauthConsent.id }).from(oauthConsent).where(and(
    eq(oauthConsent.clientId, grant.clientId),
    eq(oauthConsent.userId, grant.userId),
    eq(oauthConsent.referenceId, grant.organizationId),
  )).limit(1);
  if (consent === undefined) return;
  yield* drizzle.insert(oauthConsentSession).values({ consentId: consent.id, sessionId }).onConflictDoNothing();
});

/**
 * Run after a member signs a browser out, once its session row is gone: disconnects each agent of theirs that no browser
 * still signed in allowed on /connect, which ends its grant for `/mcp` and every credential under it. So signing out
 * disconnects the agents allowed from that browser, and any whose browser's sign-in has since run out. A sign-in that
 * runs out on its own calls nothing, so an agent outlives a browser that just stopped visiting. A consent with no
 * recorded browser is left alone.
 */
export const disconnectSignedOutAgents = Effect.fn("Identity.disconnectSignedOutAgents")(function* (userId: string) {
  const { drizzle } = yield* Database;
  const allowedFrom = drizzle.select({ sessionId: oauthConsentSession.sessionId })
    .from(oauthConsentSession)
    .where(eq(oauthConsentSession.consentId, oauthConsent.id));
  const stillSignedIn = drizzle.select({ sessionId: oauthConsentSession.sessionId })
    .from(oauthConsentSession)
    .innerJoin(session, eq(session.id, oauthConsentSession.sessionId))
    .where(and(eq(oauthConsentSession.consentId, oauthConsent.id), gt(session.expiresAt, new Date())));
  yield* drizzle.delete(oauthConsent).where(and(
    eq(oauthConsent.userId, userId),
    exists(allowedFrom),
    notExists(stillSignedIn),
  ));
});
