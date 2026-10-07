import "@tanstack/react-start/server-only";
import { Context, Data, Effect, Layer, Redacted, Schema } from "effect";
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
        eventKey: Redacted.value(config.inngest.eventKey),
        signingKey: Redacted.value(config.inngest.signingKey),
      }),
  ),
);

/** What Inngest says of one run; `missing` when it has no such run. */
export type InngestRunStatus = "running" | "completed" | "failed" | "cancelled" | "missing";

const RunStatusBody = Schema.Struct({ data: Schema.Struct({ status: Schema.String }) });
const ENDED_RUN_STATUSES = ["completed", "failed", "cancelled"] as const;
const runStatusOf = (status: string | null): InngestRunStatus =>
  status === null ? "missing" : (ENDED_RUN_STATUSES.find((ended) => ended === status.toLowerCase()) ?? "running");

/** GET /v1/runs/{id} on Inngest's REST API. */
export const inngestRunStatus = (runId: string) =>
  Effect.gen(function* () {
    const config = yield* AppConfig;
    const url = new URL(`/v1/runs/${encodeURIComponent(runId)}`, config.inngest.baseUrl);
    const response = yield* Effect.tryPromise(() =>
      fetch(url, { headers: { authorization: `Bearer ${Redacted.value(config.inngest.signingKey)}` } }));
    if (response.status === 404) return runStatusOf(null);
    if (!response.ok) return yield* Effect.fail(new Error(`Inngest answered ${response.status} for run ${runId}.`));
    const body = yield* Effect.tryPromise(() => response.json()).pipe(Effect.flatMap(Schema.decodeUnknownEffect(RunStatusBody)));
    return runStatusOf(body.data.status);
  });
