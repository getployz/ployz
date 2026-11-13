import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { beginPasskeyRegistration, finishPasskeyRegistration, listPasskeys, serversWithoutPasskey } from "#/modules/passkeys/passkeys.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

const InOrganization = Schema.Struct({ organizationSlug: Schema.String });

export const listPasskeysServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(InOrganization))
  .handler(({ context, data }) => runActor(context, listPasskeys(context.actor, data.organizationSlug)));

export const beginPasskeyServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(InOrganization))
  .handler(({ context, data }) => runActor(context, beginPasskeyRegistration(context.actor, data.organizationSlug)));

/** The browser's `navigator.credentials.create` answer as JSON text; the WebAuthn verifier checks all of it. */
export const finishPasskeyServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.String, response: Schema.String.check(Schema.isMaxLength(64_000)) })))
  .handler(({ context, data }) =>
    runActor(context, finishPasskeyRegistration(context.actor, data.organizationSlug, data.response)));

export const serversWithoutPasskeyServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(InOrganization))
  .handler(({ context, data }) => runActor(context, serversWithoutPasskey(context.actor, data.organizationSlug)));
