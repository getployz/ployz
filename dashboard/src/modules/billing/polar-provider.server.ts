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

export type PolarService =
  | { readonly mode: "self_hosted" }
  | {
      readonly mode: "hosted";
      readonly productId: string;
      readonly listActiveSubscriptions: (
        organizationId: string,
      ) => Effect.Effect<readonly PolarSubscription[], PolarFailure>;
      /** A customer portal session for the user who paid, where plans are changed or cancelled. */
      readonly createCustomerPortal: (input: {
        readonly externalCustomerId: string;
        readonly returnUrl: string;
      }) => Effect.Effect<{ readonly customerPortalUrl: string }, PolarFailure>;
    };

export class Polar extends Context.Service<Polar, PolarService>()(
  "ployz/Polar",
) {}

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
    createCustomerPortal: (input) =>
      call(
        "create customer portal",
        () =>
          client.customerSessions.create({
            external_customer_id: input.externalCustomerId,
            return_url: input.returnUrl,
          }),
        CustomerPortal,
      ),
  };
}

export const PolarLive = Layer.effect(
  Polar,
  Effect.map(AppConfig, (config) => makePolarService(config.polar)),
);
