import "@tanstack/react-start/server-only";
import { makePolarClient, type Polar as PolarSdk } from "#/modules/billing/polar-api";
import { Context, Data, Effect, Layer, Option, Schema } from "effect";
import type { PolarConfiguration } from "#/server/config.server";
import { AppConfig } from "#/server/config.server";
import { PolarSubscription } from "#/modules/billing/billing";

export class PolarFailure extends Data.TaggedError("PolarFailure")<{
  readonly operation: string;
  readonly code: "request_failed" | "invalid_response";
  readonly retriable: boolean;
  readonly message: string;
  readonly cause: unknown;
}> {}

export type CreatePolarCheckout = {
  readonly successUrl: string;
  readonly embedOrigin: string;
  readonly externalCustomerId: string;
  readonly customerEmail: string;
  readonly customerName: string;
  readonly referenceId: string;
};

export type PolarService =
  | { readonly mode: "self_hosted" }
  | {
      readonly mode: "hosted";
      readonly productId: string;
      readonly listActiveSubscriptions: (
        organizationId: string,
      ) => Effect.Effect<readonly PolarSubscription[], PolarFailure>;
      readonly createCheckout: (
        input: CreatePolarCheckout,
      ) => Effect.Effect<{ readonly url: string }, PolarFailure>;
      /**
       * A customer portal session for the user who paid, where plans are changed or cancelled. Failing that, the
       * portal of the customer holding their email: Polar keys customers by email and never re-points `external_id`,
       * so after the user deleted their account and signed up again, their checkout lands on the old customer.
       */
      readonly createCustomerPortal: (input: {
        readonly externalCustomerId: string;
        readonly customerEmail: string;
        readonly returnUrl: string;
      }) => Effect.Effect<{ readonly customerPortalUrl: string }, PolarFailure>;
    };

export class Polar extends Context.Service<Polar, PolarService>()(
  "ployz/Polar",
) {}

const Checkout = Schema.Struct({ url: Schema.String });
const CustomerPortal = Schema.Struct({
  customerPortalUrl: Schema.String,
}).pipe(Schema.encodeKeys({ customerPortalUrl: "customer_portal_url" }));
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

  const client = sdk ?? makePolarClient(config);
  return {
    mode: "hosted",
    productId: config.productId,
    listActiveSubscriptions: (organizationId) =>
      call(
        "list active subscriptions",
        async () => {
          const items: unknown[] = [];
          for await (const subscription of client.subscriptions.iterList({
            active: true,
            limit: 100,
            metadata: { referenceId: organizationId },
          })) {
            items.push(subscription);
          }
          return items;
        },
        Schema.Array(PolarSubscription),
      ),
    createCheckout: (input) =>
      call(
        "create checkout",
        () =>
          client.checkouts.create({
            products: [config.productId],
            success_url: input.successUrl,
            embed_origin: input.embedOrigin,
            external_customer_id: input.externalCustomerId,
            customer_email: input.customerEmail,
            customer_name: input.customerName,
            metadata: { referenceId: input.referenceId },
          }),
        Checkout,
      ),
    createCustomerPortal: (input) =>
      call(
        "create customer portal",
        () =>
          client.customerSessions
            .create({
              external_customer_id: input.externalCustomerId,
              return_url: input.returnUrl,
            })
            .catch(async (cause: unknown) => {
              const { items } = await client.customers.list({ email: input.customerEmail, limit: 1 });
              const customer = items[0];
              if (customer === undefined) throw cause;
              return client.customerSessions.create({ customer_id: customer.id, return_url: input.returnUrl });
            }),
        CustomerPortal,
      ),
  };
}

export const PolarLive = Layer.effect(
  Polar,
  Effect.map(AppConfig, (config) => makePolarService(config.polar)),
);
