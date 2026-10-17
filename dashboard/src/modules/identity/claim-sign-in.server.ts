import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { hashClaimSecret } from "#/modules/identity/claim-required.server";
import { verification } from "#/modules/identity/tables";
import { Database } from "#/server/database.server";

export const CLAIM_SIGN_IN_COOKIE = "ployz_claim_sign_in";
const MAX_AGE_SECONDS = 10 * 60;
const CLAIM_PATH = /^\/claim\/([\w-]+)(?:[?#]|$)/;

/** The hash of the claim secret a sign-in returns to, or null when its callback is not a claim page. */
export function claimSecretHashOf(callbackURL: string) {
  const secret = CLAIM_PATH.exec(callbackURL)?.[1];
  return secret === undefined ? null : hashClaimSecret(secret);
}

/** The cookie that marks a sign-in the claim page started, or null when the callback is not a claim page. */
export function claimSignInCookie(callbackURL: string, now: Date, secure: boolean) {
  const hash = claimSecretHashOf(callbackURL);
  if (hash === null) return null;
  const value = `${now.getTime()}.${hash}`;
  return [`${CLAIM_SIGN_IN_COOKIE}=${value}`, "Path=/", `Max-Age=${MAX_AGE_SECONDS}`, "HttpOnly", "SameSite=Lax", ...(secure ? ["Secure"] : [])].join("; ");
}

/** Whether the cookie says the claim page for this secret started a sign-in recently, so the page claims on return. */
export function startedClaimSignIn(cookie: string | undefined, secret: string, now: Date) {
  const [startedAtMs, hash] = cookie?.split(".") ?? [];
  const age = now.getTime() - Number(startedAtMs);
  return hash === hashClaimSecret(secret) && age >= 0 && age <= MAX_AGE_SECONDS * 1000;
}

const bindingIdentifier = (userId: string, claimSecretHash: string) => `claim-sign-in:${userId}:${claimSecretHash}`;

/**
 * Records the Organization a sign-up made, when the server-signed OAuth state says the claim page started that
 * sign-in. The claim later removes this Organization and no other.
 */
export const bindClaimSignInOrganization = Effect.fn("Identity.bindClaimSignInOrganization")(
  function* (userId: string, organizationId: string, claimSecretHash: string | null) {
    if (claimSecretHash === null) return;
    const { drizzle } = yield* Database;
    yield* drizzle.insert(verification).values({
      identifier: bindingIdentifier(userId, claimSecretHash),
      value: organizationId,
      expiresAt: new Date(Date.now() + MAX_AGE_SECONDS * 1000),
    });
  },
);

/** The Organization this person's sign-in from this claim page made, consumed so it is removed at most once. */
export const takeClaimSignInOrganization = Effect.fn("Identity.takeClaimSignInOrganization")(function* (userId: string, secret: string) {
  const { drizzle } = yield* Database;
  const bindings = yield* drizzle.delete(verification)
    .where(eq(verification.identifier, bindingIdentifier(userId, hashClaimSecret(secret))))
    .returning({ organizationId: verification.value, expiresAt: verification.expiresAt });
  const now = new Date();
  return bindings.find(({ expiresAt }) => expiresAt > now)?.organizationId ?? null;
});
