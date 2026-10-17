import { createServerFn } from "@tanstack/react-start";
import { deleteCookie, getCookie, getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { getAuthSessionEffect } from "#/auth/auth.server";
import { claimOrganization, finishClaimSignIn, readClaim } from "#/modules/identity/claim.server";
import { CLAIM_SIGN_IN_COOKIE, startedClaimSignIn } from "#/modules/identity/claim-sign-in.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const ClaimInput = Schema.Struct({ secret: Schema.String });

/**
 * Takes the headers because getRequest() is only the caller's request before the Effect first yields. A person back
 * from the GitHub sign-in this page started keeps the sandbox here, so one click claims it.
 */
const readClaimAs = (headers: Headers, secret: string, signingIn: boolean) =>
  getAuthSessionEffect(headers).pipe(
    Effect.flatMap((session) => {
      if (session === null) return readClaim(secret, null);
      const actor = { userId: session.user.id };
      return signingIn && session.user.isAnonymous !== true ? finishClaimSignIn(secret, actor) : readClaim(secret, actor);
    }),
  );

export const readClaimServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware])
  .validator(strictValidator(ClaimInput))
  .handler(({ data }) => {
    const request = getRequest();
    const signingIn = startedClaimSignIn(getCookie(CLAIM_SIGN_IN_COOKIE), data.secret, new Date());
    if (signingIn) deleteCookie(CLAIM_SIGN_IN_COOKIE, { path: "/" });
    return runActor({ signal: request.signal }, readClaimAs(request.headers, data.secret, signingIn));
  });

export const claimOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ClaimInput))
  .handler(({ data, context }) => runActor(context, claimOrganization(context.actor, data.secret)));
