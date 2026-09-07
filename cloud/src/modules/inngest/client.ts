import "@tanstack/react-start/server-only";
import { Context, Data, Effect, Layer, Redacted } from "effect";
import { Inngest, type ClientOptions, type GetStepTools } from "inngest";
import type { InngestSendableEvent } from "#/modules/inngest/events";
import { AppConfig } from "#/server/config.server";

export type PloyzInngest = Inngest<ClientOptions>;
export type PloyzStepTools = GetStepTools<PloyzInngest>;

export class InngestClient extends Context.Service<
  InngestClient,
  PloyzInngest
>()(
  "ployz/InngestClient",
) {}

export class InngestEventSendError extends Data.TaggedError(
  "InngestEventSendError",
)<{ readonly cause: unknown }> {}

function isEventBatch(
  event: InngestSendableEvent | ReadonlyArray<InngestSendableEvent>,
): event is ReadonlyArray<InngestSendableEvent> {
  return Array.isArray(event);
}

export const sendInngestEvent = Effect.fn("Inngest.sendEvent")(
  function* (
    event: InngestSendableEvent | ReadonlyArray<InngestSendableEvent>,
  ) {
    const inngest = yield* InngestClient;
    yield* Effect.tryPromise({
      try: () =>
        inngest.send(isEventBatch(event) ? Array.from(event) : event),
      catch: (cause) => new InngestEventSendError({ cause }),
    });
  },
);

export const InngestLive = Layer.effect(
  InngestClient,
  Effect.map(
    AppConfig,
    (config) =>
      new Inngest({
        id: "ployz-cloud",
        eventKey: config.inngest.eventKey === undefined
          ? undefined
          : Redacted.value(config.inngest.eventKey),
        signingKey: config.inngest.signingKey === undefined
          ? undefined
          : Redacted.value(config.inngest.signingKey),
        signingKeyFallback: config.inngest.signingKeyFallback === undefined
          ? undefined
          : Redacted.value(config.inngest.signingKeyFallback),
      }),
  ),
);
