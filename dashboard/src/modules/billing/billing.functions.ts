import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import {
  createCustomerPortal,
  createEmbeddedCheckout,
  getBillingState,
} from "#/modules/billing/billing.server";
import { getCustomDomainsAllowed, syncCustomDomainCapability } from "#/modules/billing/custom-domain-capability";
import { trimmedString } from "#/lib/schema";
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

export const createCustomerPortalServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(BillingStateRequest))
  .handler(({ context, data }) =>
    runActor(context, createCustomerPortal(context.actor, data)),
  );

export const getCustomDomainsAllowedServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(BillingStateRequest))
  .handler(({ context, data }) =>
    runActor(context, getCustomDomainsAllowed(context.actor, data)),
  );

export const syncCustomDomainCapabilityServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(BillingStateRequest))
  .handler(({ context, data }) =>
    runActor(context, syncCustomDomainCapability(context.actor, data)),
  );
