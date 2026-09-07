import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { BillingPlan } from "#/modules/billing/billing";
import {
  createEmbeddedCheckout,
  getBillingState,
  previewSubscriptionPlanChange,
  updateSubscriptionPlan,
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
const BillingPlanRequest = Schema.Struct({
  organizationSlug: trimmedString({ minLength: 1 }),
  plan: BillingPlan,
});

export const getBillingStateServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .validator(strictValidator(BillingStateRequest))
  .handler(({ context, data }) =>
    runActor(context, getBillingState(context.actor, data)),
  );

export const previewSubscriptionPlanChangeServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(BillingPlanRequest))
  .handler(({ context, data }) =>
    runActor(context, previewSubscriptionPlanChange(context.actor, data)),
  );

export const updateSubscriptionPlanServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(BillingPlanRequest))
  .handler(({ context, data }) =>
    runActor(context, updateSubscriptionPlan(context.actor, data)),
  );

export const createEmbeddedCheckoutServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(BillingPlanRequest))
  .handler(({ context, data }) =>
    runActor(context, createEmbeddedCheckout(context.actor, data)),
  );
