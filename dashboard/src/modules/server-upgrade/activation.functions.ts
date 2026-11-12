import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { requireAccess } from "#/modules/organization/access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { ActivateServersInput, GenerateRecipientInput, InstallRecipientInput } from "./activation";
import { activateServers, generateRecipient, installRecipient, readActivation } from "./activation.server";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

/** Each Server's own account of its activation, through Inspect. */
export const readActivationServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const { organization } = yield* requireAccess(context.actor, data.organizationSlug, "servers:read");
    return yield* readActivation(organization.id);
  })));

/** Hand each Server the starting set the person's passkey answered. */
export const activateServersServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(ActivateServersInput))
  .handler(({ context, data }) => runActor(context, activateServers(context.actor, data.organizationSlug, data.startingSet)));

/** Have one Server that activated generate the new key the person's second prompt names. */
export const generateRecipientServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(GenerateRecipientInput))
  .handler(({ context, data }) => runActor(context, generateRecipient(context.actor, data.organizationSlug, data.operation)));

/** Install the new key the person's passkey named on every Server the rotation names. */
export const installRecipientServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(InstallRecipientInput))
  .handler(({ context, data }) => runActor(context, installRecipient(context.actor, data)));
