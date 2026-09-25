import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import {
  createEmbeddedCheckout,
  getBillingState,
} from "#/modules/billing/billing.server";
import { trimmedString } from "#/modules/environment-design/schema";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;
const BillingStateRequest = Schema.Struct({
  organizationSlug: trimmedString({ minLength: 1 }),
});

export const getBillingStateServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(BillingStateRequest))
  .handler(({ context, data }) =>
    runActor(context, getBillingState(context.actor, data)),
  );

export const createEmbeddedCheckoutServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(BillingStateRequest))
  .handler(({ context, data }) =>
    runActor(context, createEmbeddedCheckout(context.actor, data)),
  );
