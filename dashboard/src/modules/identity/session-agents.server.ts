import "@tanstack/react-start/server-only";
import { and, eq, exists, gt, inArray, isNull, notExists, notInArray, or, sql } from "drizzle-orm";
import { Effect, Option, Semaphore } from "effect";
import { oauthConsent, oauthConsentSession } from "#/modules/identity/oauth-tables";
import { session } from "#/modules/identity/tables";
import { whileLocked } from "#/server/advisory-lock.server";
import { Database } from "#/server/database.server";

/**
 * Takes, in the current transaction, the lock that orders a member's Allows against their sign-outs, so a grant is never
 * forgotten, reused or linked against a stale read of its browsers. A transaction takes it before any row lock.
 */
export const lockAgentGrants = Effect.fn("Identity.lockAgentGrants")(function* (userId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`agent-grants:${userId}`}))`);
  return true;
});

const grantHolders = Semaphore.makeUnsafe(4);

/** Runs `body`, whose queries each commit on their own, while member `userId`'s grants are locked. */
export const withAgentGrants = <A, E, R>(userId: string, body: Effect.Effect<A, E, R>) =>
  whileLocked(grantHolders, lockAgentGrants(userId), body).pipe(
    Effect.flatMap(Option.match({ onNone: () => Effect.die("a blocking lock answered that it is taken"), onSome: Effect.succeed })),
  );

/**
 * Records that `sessionId` allowed the consent standing for this client, user and Organization on /connect. /connect
 * hands the agent its code only after this, so a grant is never usable before it has a browser. False when there is no
 * such consent, or the browser was signed out meanwhile: the share lock waits out a sign-out deleting it, and a link
 * that lands after a sign-out's marks is marked here, so a code allowed during a sign-out never acts.
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
  const link = and(eq(oauthConsentSession.consentId, consent.id), eq(oauthConsentSession.sessionId, sessionId));
  yield* drizzle.insert(oauthConsentSession).values({ consentId: consent.id, sessionId }).onConflictDoNothing();
  const [live] = yield* drizzle.select({ id: session.id }).from(session)
    .where(and(eq(session.id, sessionId), gt(session.expiresAt, new Date())))
    .for("share");
  if (live !== undefined) return true;
  yield* drizzle.update(oauthConsentSession).set({ signedOutAt: new Date() }).where(link);
  return false;
});

/**
 * A disconnected grant: one whose every recorded browser was signed out. Allow deletes it first, so better-auth writes a
 * new grant rather than reviving the old one, and the credentials signing out stopped stay stopped.
 */
export const forgetSignedOutGrant = Effect.fn("Identity.forgetSignedOutGrant")(function* (
  grant: { readonly clientId: string; readonly userId: string; readonly organizationId: string },
) {
  const { drizzle } = yield* Database;
  const browsers = drizzle.select({ sessionId: oauthConsentSession.sessionId }).from(oauthConsentSession)
    .where(eq(oauthConsentSession.consentId, oauthConsent.id));
  const signedIn = drizzle.select({ sessionId: oauthConsentSession.sessionId }).from(oauthConsentSession)
    .where(and(eq(oauthConsentSession.consentId, oauthConsent.id), isNull(oauthConsentSession.signedOutAt)));
  yield* drizzle.delete(oauthConsent).where(and(
    eq(oauthConsent.clientId, grant.clientId),
    eq(oauthConsent.userId, grant.userId),
    eq(oauthConsent.referenceId, grant.organizationId),
    exists(browsers),
    notExists(signedIn),
  ));
});

/**
 * Whether consent `consentId` may mint tokens: a browser that allowed it hasn't been signed out, or it has no browser
 * recorded, in which case `oauthCaller` holds its tokens to their own session as every grant once was.
 */
export const grantActs = Effect.fn("Identity.grantActs")(function* (consentId: string) {
  const { drizzle } = yield* Database;
  const links = yield* drizzle.select({ signedOutAt: oauthConsentSession.signedOutAt })
    .from(oauthConsentSession)
    .where(eq(oauthConsentSession.consentId, consentId));
  return links.length === 0 || links.some((link) => link.signedOutAt === null);
});

/**
 * A member signing browser `sessionId` out: marks it signed out on every grant it allowed, and marks every other browser
 * of theirs whose sign-in has already run out, so an agent whose browser lapsed ends at their next sign-out anywhere.
 * Call it before the session row goes, so a failed mark leaves the browser signed in rather than its agents connected,
 * and again once it's gone, for a link /connect wrote in between. It only ever marks; a grant stops acting once all its browsers are
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
