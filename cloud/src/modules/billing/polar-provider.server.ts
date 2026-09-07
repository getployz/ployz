import "@tanstack/react-start/server-only";
import { Polar as PolarSdk } from "@polar-sh/sdk";
import { Context, Data, Effect, Layer, Option, Redacted, Schema } from "effect";
import type { PolarConfiguration } from "#/server/config.server";
import { AppConfig } from "#/server/config.server";
import {
  PolarSubscription,
  type BillingPlan,
  type ProductIds,
} from "#/modules/billing/billing";

export class PolarFailure extends Data.TaggedError("PolarFailure")<{
  readonly operation: string;
  readonly code: "request_failed" | "invalid_response";
  readonly retriable: boolean;
  readonly message: string;
  readonly cause: unknown;
}> {}

export type PolarPrice =
  | { readonly kind: "free"; readonly currency: string }
  | { readonly kind: "fixed"; readonly currency: string; readonly amount: number }
  | { readonly kind: "unsupported"; readonly currency: string };

export type CreatePolarCheckout = {
  readonly productId: string;
  readonly successUrl: string;
  readonly embedOrigin: string;
  readonly externalCustomerId: string;
  readonly customerEmail: string;
  readonly customerName: string;
  readonly referenceId: string;
  readonly subscriptionId?: string;
};

export type PolarService =
  | { readonly mode: "self_hosted" }
  | {
      readonly mode: "hosted";
      readonly productIds: ProductIds;
      readonly listActiveSubscriptions: (
        organizationId: string,
      ) => Effect.Effect<readonly PolarSubscription[], PolarFailure>;
      readonly createFreeSubscription: (input: {
        readonly organizationId: string;
        readonly userId: string;
      }) => Effect.Effect<PolarSubscription, PolarFailure>;
      readonly getProductPrices: (
        productId: string,
      ) => Effect.Effect<readonly PolarPrice[], PolarFailure>;
      readonly updateSubscriptionPlan: (input: {
        readonly subscriptionId: string;
        readonly plan: BillingPlan;
      }) => Effect.Effect<PolarSubscription, PolarFailure>;
      readonly createCheckout: (
        input: CreatePolarCheckout,
      ) => Effect.Effect<{ readonly url: string }, PolarFailure>;
    };

export class Polar extends Context.Service<Polar, PolarService>()(
  "ployz/Polar",
) {}

const ProviderPrice = Schema.Struct({
  isArchived: Schema.Boolean,
  priceCurrency: Schema.String,
  amountType: Schema.String,
  priceAmount: Schema.optionalKey(Schema.Finite),
});
const Checkout = Schema.Struct({ url: Schema.String });
const Product = Schema.Struct({
  prices: Schema.Array(ProviderPrice),
});
const ProviderErrorEvidence = Schema.Struct({
  status: Schema.optionalKey(Schema.Finite),
  statusCode: Schema.optionalKey(Schema.Finite),
});

function providerFailure(operation: string, cause: unknown) {
  const invalidResponse = Schema.isSchemaError(cause);
  const evidence = Schema.decodeUnknownOption(ProviderErrorEvidence)(cause);
  const descriptor = Option.isSome(evidence) ? evidence.value : undefined;
  const status = descriptor?.status ?? descriptor?.statusCode;
  return new PolarFailure({
    operation,
    code: invalidResponse ? "invalid_response" : "request_failed",
    retriable:
      !invalidResponse &&
      (status === undefined || status === 429 || status >= 500),
    message: `Polar ${operation} failed.`,
    cause,
  });
}

function call<S extends Schema.ConstraintDecoder<unknown>>(
  operation: string,
  run: () => Promise<object>,
  schema: S,
): Effect.Effect<S["Type"], PolarFailure> {
  return Effect.tryPromise({
    try: run,
    catch: (cause) => providerFailure(operation, cause),
  }).pipe(
    Effect.flatMap((value) => Schema.decodeUnknownEffect(schema)(value)),
    Effect.catchIf(Schema.isSchemaError, (cause) =>
      Effect.fail(providerFailure(operation, cause)),
    ),
  );
}

export function makePolarService(
  config: PolarConfiguration,
  sdk?: PolarSdk,
): PolarService {
  if (config.mode === "self_hosted") return { mode: "self_hosted" };

  const client =
    sdk ??
    new PolarSdk({
      accessToken: Redacted.value(config.accessToken),
      server: config.server,
    });
  const productIds = config.productIds;

  return {
    mode: "hosted",
    productIds,
    listActiveSubscriptions: (organizationId) =>
      call(
        "list active subscriptions",
        async () => {
          const pages = await client.subscriptions.list({
            active: true,
            limit: 100,
            metadata: { referenceId: organizationId },
          });
          const items: unknown[] = [];
          for await (const page of pages) {
            items.push(...page.result.items);
          }
          return items;
        },
        Schema.Array(PolarSubscription),
      ),
    createFreeSubscription: (input) =>
      call(
        "create free subscription",
        () =>
          client.subscriptions.create({
            externalCustomerId: input.userId,
            productId: productIds.free,
            metadata: { referenceId: input.organizationId },
          }),
        PolarSubscription,
      ),
    getProductPrices: (productId) =>
      call("get product prices", () => client.products.get({ id: productId }), Product).pipe(
        Effect.map((product) =>
          product.prices.flatMap((price): PolarPrice[] => {
            if (price.isArchived) return [];
            if (price.amountType === "free") {
              return [{ kind: "free", currency: price.priceCurrency }];
            }
            if (
              price.amountType === "fixed" &&
              price.priceAmount !== undefined
            ) {
              return [{
                kind: "fixed",
                currency: price.priceCurrency,
                amount: price.priceAmount,
              }];
            }
            return [{ kind: "unsupported", currency: price.priceCurrency }];
          }),
        ),
      ),
    updateSubscriptionPlan: (input) =>
      call(
        "update subscription",
        () =>
          client.subscriptions.update({
            id: input.subscriptionId,
            subscriptionUpdate: {
              productId: productIds[input.plan],
              prorationBehavior: "invoice",
            },
          }),
        PolarSubscription,
      ),
    createCheckout: (input) =>
      call(
        "create checkout",
        () =>
          client.checkouts.create({
            products: [input.productId],
            successUrl: input.successUrl,
            embedOrigin: input.embedOrigin,
            externalCustomerId: input.externalCustomerId,
            customerEmail: input.customerEmail,
            customerName: input.customerName,
            metadata: { referenceId: input.referenceId },
            subscriptionId: input.subscriptionId,
          }),
        Checkout,
      ),
  };
}

export const PolarLive = Layer.effect(
  Polar,
  Effect.map(AppConfig, (config) => makePolarService(config.polar)),
);
