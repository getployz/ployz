import "@tanstack/react-start/server-only";
import { and, eq, gt, inArray, isNull, notExists, notInArray, or, sql } from "drizzle-orm";
import { Effect, Option, Semaphore } from "effect";
import { oauthAccessToken, oauthConsent, oauthConsentSession, oauthRefreshToken } from "#/modules/identity/oauth-tables";
import { session } from "#/modules/identity/tables";
import { whileLocked } from "#/server/advisory-lock.server";
import { Database } from "#/server/database.server";

// Holding a member's grants pins a connection while its work takes more, so this bounds how many hold or wait at once.
const grantHolders = Semaphore.makeUnsafe(4);

/** Takes the lock that orders a member's Allows against their sign-outs, held until the current transaction ends. */
const lockAgentGrants = Effect.fn("Identity.lockAgentGrants")(function* (userId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`agent-grants:${userId}`}))`);
  return true;
});

/**
 * Runs `body`, whose queries each commit on their own, while member `userId`'s grants are locked, so a grant is never
 * forgotten, reused or linked against a stale read of its browsers.
 */
export const withAgentGrants = <A, E, R>(userId: string, body: Effect.Effect<A, E, R>) =>
  whileLocked(grantHolders, lockAgentGrants(userId), body).pipe(
    Effect.flatMap(Option.match({ onNone: () => Effect.die("a blocking lock answered that it is taken"), onSome: Effect.succeed })),
  );

/** Runs `body` in a transaction that first locks member `userId`'s grants, before it takes any row lock. */
export const transactionWithAgentGrants = <A, E, R>(userId: string, body: Effect.Effect<A, E, R>) =>
  grantHolders.withPermits(1)(Effect.gen(function* () {
    const database = yield* Database;
    return yield* database.transaction(Effect.andThen(lockAgentGrants(userId), body));
  }));

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
 * A grant no signed-in browser stands behind: every recorded browser was signed out, or none is recorded, as for one
 * signed out before browsers were. Allow deletes it first, so better-auth writes a new grant rather than reviving the
 * old one, and the credentials signing out stopped stay stopped. An unrecorded grant still acting on its token's
 * session gets a new one too, and the agent holding the old one connects again.
 */
export const forgetSignedOutGrant = Effect.fn("Identity.forgetSignedOutGrant")(function* (
  grant: { readonly clientId: string; readonly userId: string; readonly organizationId: string },
) {
  const { drizzle } = yield* Database;
  const signedIn = drizzle.select({ sessionId: oauthConsentSession.sessionId }).from(oauthConsentSession)
    .where(and(eq(oauthConsentSession.consentId, oauthConsent.id), isNull(oauthConsentSession.signedOutAt)));
  yield* drizzle.delete(oauthConsent).where(and(
    eq(oauthConsent.clientId, grant.clientId),
    eq(oauthConsent.userId, grant.userId),
    eq(oauthConsent.referenceId, grant.organizationId),
    notExists(signedIn),
  ));
});

/**
 * A grant may mint tokens and appear as connected: a browser that allowed it hasn't been signed out, or it has no browser
 * recorded, in which case `oauthCaller` holds its tokens to their own session as every grant once was.
 */
export const grantStands = sql`(not exists (select from ${oauthConsentSession} where ${oauthConsentSession.consentId} = ${oauthConsent.id})
  or exists (select from ${oauthConsentSession} where ${oauthConsentSession.consentId} = ${oauthConsent.id} and ${oauthConsentSession.signedOutAt} is null))`;

/** Whether consent `consentId` may mint tokens, per `grantStands`. */
export const grantActs = Effect.fn("Identity.grantActs")(function* (consentId: string) {
  const { drizzle } = yield* Database;
  const [standing] = yield* drizzle.select({ id: oauthConsent.id }).from(oauthConsent)
    .where(and(eq(oauthConsent.id, consentId), grantStands)).limit(1);
  return standing !== undefined;
});

/**
 * A member signing browser `sessionId` out: marks it signed out on every grant it allowed, and marks every other browser
 * of theirs whose sign-in has already run out, so an agent whose browser lapsed ends at their next sign-out anywhere.
 * It deletes the refresh tokens those browsers' agents hold too, so each agent stops by its own browser even when
 * another browser allowed the same grant (`oauthCaller` refuses their access tokens by the browser they name).
 * Call it before the session row goes, so a failed mark leaves the browser signed in rather than its agents connected,
 * and again once it's gone, for a link /connect wrote in between. It only ever marks and deletes, so concurrent
 * sign-outs can't each leave a grant to the other.
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
  // A refresh token whose browser better-auth already cleared names none; its browser's sign-in had run out.
  const stopped = drizzle.select({ id: oauthRefreshToken.id }).from(oauthRefreshToken).where(and(
    eq(oauthRefreshToken.userId, userId),
    or(eq(oauthRefreshToken.sessionId, sessionId), isNull(oauthRefreshToken.sessionId), notInArray(oauthRefreshToken.sessionId, signedIn)),
  ));
  yield* drizzle.delete(oauthAccessToken).where(inArray(oauthAccessToken.refreshId, stopped));
  yield* drizzle.delete(oauthRefreshToken).where(inArray(oauthRefreshToken.id, stopped));
});
