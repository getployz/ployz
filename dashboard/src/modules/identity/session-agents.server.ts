import "@tanstack/react-start/server-only";
import { and, eq, gt, inArray, isNull, notInArray, or } from "drizzle-orm";
import { Effect } from "effect";
import { oauthConsent, oauthConsentSession } from "#/modules/identity/oauth-tables";
import { session } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";

/**
 * Records that `sessionId` allowed the consent standing for this client, user and Organization on /connect. /connect
 * hands the agent its code only after this, so a grant is never usable before it has a browser.
 */
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
  if (consent === undefined) return false;
  yield* drizzle.insert(oauthConsentSession).values({ consentId: consent.id, sessionId }).onConflictDoNothing();
  return true;
});

/** Whether a browser that allowed consent `consentId` hasn't been signed out, which a grant needs to act. */
export const grantSignedIn = Effect.fn("Identity.grantSignedIn")(function* (consentId: string) {
  const { drizzle } = yield* Database;
  const [link] = yield* drizzle.select({ sessionId: oauthConsentSession.sessionId })
    .from(oauthConsentSession)
    .where(and(eq(oauthConsentSession.consentId, consentId), isNull(oauthConsentSession.signedOutAt)))
    .limit(1);
  return link !== undefined;
});

/**
 * A member signing browser `sessionId` out: marks it signed out on every grant it allowed, and marks every other browser
 * of theirs whose sign-in has already run out, so an agent whose browser lapsed ends at their next sign-out anywhere.
 * Call it before the session row goes, in the same transaction where there is one, so a failed mark leaves the browser
 * signed in rather than its agents connected. It only ever marks; a grant stops acting once all its browsers are
 * marked (`oauthCaller`), so concurrent sign-outs can't each leave the grant to the other.
 */
export const signOutSessionAgents = Effect.fn("Identity.signOutSessionAgents")(function* (userId: string, sessionId: string) {
  const { drizzle } = yield* Database;
  const consents = drizzle.select({ id: oauthConsent.id }).from(oauthConsent).where(eq(oauthConsent.userId, userId));
  const signedIn = drizzle.select({ id: session.id }).from(session).where(and(eq(session.userId, userId), gt(session.expiresAt, new Date())));
  yield* drizzle.update(oauthConsentSession)
    .set({ signedOutAt: new Date() })
    .where(and(
      isNull(oauthConsentSession.signedOutAt),
      inArray(oauthConsentSession.consentId, consents),
      or(eq(oauthConsentSession.sessionId, sessionId), notInArray(oauthConsentSession.sessionId, signedIn)),
    ));
});
