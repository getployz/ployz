import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { getAuthSessionEffect } from "#/auth/auth.server";
import { claimOrganization, readClaim } from "#/modules/identity/claim.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const ClaimInput = Schema.Struct({ secret: Schema.String });

/** Takes the headers because getRequest() is only the caller's request before the Effect first yields. */
const readClaimAs = (headers: Headers, secret: string) =>
  getAuthSessionEffect(headers).pipe(
    Effect.flatMap((session) => readClaim(secret, session === null ? null : { userId: session.user.id })),
  );

export const readClaimServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware])
  .validator(strictValidator(ClaimInput))
  .handler(({ data }) => {
    const request = getRequest();
    return runActor({ signal: request.signal }, readClaimAs(request.headers, data.secret));
  });

export const claimOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ClaimInput))
  .handler(({ data, context }) => runActor(context, claimOrganization(context.actor, data.secret)));
