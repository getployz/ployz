import "@tanstack/react-start/server-only";
import { hashClaimSecret } from "#/modules/identity/claim-required.server";

/** The GitHub sign-in the claim page started: only the Organization it made is a cleanup candidate. */
export type ClaimSignIn = { readonly startedAt: Date };

export const CLAIM_SIGN_IN_COOKIE = "ployz_claim_sign_in";
const MAX_AGE_SECONDS = 10 * 60;
const CLAIM_PATH = /^\/claim\/([\w-]+)(?:[?#]|$)/;

/** The cookie that marks a sign-in the claim page started, or null when the callback is not a claim page. */
export function claimSignInCookie(callbackURL: string, now: Date, secure: boolean) {
  const secret = CLAIM_PATH.exec(callbackURL)?.[1];
  if (secret === undefined) return null;
  const value = `${now.getTime()}.${hashClaimSecret(secret)}`;
  return [`${CLAIM_SIGN_IN_COOKIE}=${value}`, "Path=/", `Max-Age=${MAX_AGE_SECONDS}`, "HttpOnly", "SameSite=Lax", ...(secure ? ["Secure"] : [])].join("; ");
}

/** The sign-in a cookie names for this secret, if it is recent enough to still be the one the claim page started. */
export function readClaimSignIn(cookie: string | undefined, secret: string, now: Date): ClaimSignIn | null {
  const [startedAtMs, hash] = cookie?.split(".") ?? [];
  const startedAt = new Date(Number(startedAtMs));
  if (hash !== hashClaimSecret(secret) || Number.isNaN(startedAt.getTime())) return null;
  const age = now.getTime() - startedAt.getTime();
  return age >= 0 && age <= MAX_AGE_SECONDS * 1000 ? { startedAt } : null;
}
