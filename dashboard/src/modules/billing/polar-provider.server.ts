import "@tanstack/react-start/server-only";
import { Polar as PolarSdk } from "@polar-sh/sdk";
import { Context, Data, Effect, Layer, Option, Redacted, Schema } from "effect";
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
    };

export class Polar extends Context.Service<Polar, PolarService>()(
  "ployz/Polar",
) {}

const Checkout = Schema.Struct({ url: Schema.String });
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
  return {
    mode: "hosted",
    productId: config.productId,
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
    createCheckout: (input) =>
      call(
        "create checkout",
        () =>
          client.checkouts.create({
            products: [config.productId],
            successUrl: input.successUrl,
            embedOrigin: input.embedOrigin,
            externalCustomerId: input.externalCustomerId,
            customerEmail: input.customerEmail,
            customerName: input.customerName,
            metadata: { referenceId: input.referenceId },
          }),
        Checkout,
      ),
  };
}

export const PolarLive = Layer.effect(
  Polar,
  Effect.map(AppConfig, (config) => makePolarService(config.polar)),
);
