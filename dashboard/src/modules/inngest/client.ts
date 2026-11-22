import "@tanstack/react-start/server-only";
import { Context, Data, Effect, Layer, Redacted, Schema } from "effect";
import { Inngest, type ClientOptions, type GetStepTools } from "inngest";
import type { InngestSendableEvent } from "#/modules/inngest/events";
import { AppConfig } from "#/server/config.server";

export type PloyzInngest = Inngest<ClientOptions>;
export type PloyzStepTools = GetStepTools<PloyzInngest>;
/** The tools a handler uses, so a test hands it no more than that. */
export type StepTools<Used extends keyof PloyzStepTools = "run"> = Pick<PloyzStepTools, Used>;

const APP_ID = "ployz-cloud";

/** A function's id as Inngest reports it in `function_id`: the app id, a dash, then the id the function declares. */
export const qualifiedFunctionId = (functionId: string) => `${APP_ID}-${functionId}`;

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
        id: APP_ID,
        eventKey: Redacted.value(config.inngest.eventKey),
        signingKey: Redacted.value(config.inngest.signingKey),
      }),
  ),
);

export type InngestRunStatus = "running" | "completed" | "failed" | "cancelled" | "missing";

const RunStatusBody = Schema.Struct({ data: Schema.Struct({ status: Schema.String }) });
const ENDED_RUN_STATUSES = ["completed", "failed", "cancelled"] as const;
const runStatusOf = (status: string): InngestRunStatus =>
  ENDED_RUN_STATUSES.find((ended) => ended === status.toLowerCase()) ?? "running";

export const inngestRunStatus = (runId: string) =>
  Effect.gen(function* () {
    const config = yield* AppConfig;
    const url = new URL(`/v1/runs/${encodeURIComponent(runId)}`, config.inngest.baseUrl);
    const response = yield* Effect.tryPromise(() =>
      fetch(url, { headers: { authorization: `Bearer ${Redacted.value(config.inngest.signingKey)}` } }));
    if (response.status === 404) return "missing";
    if (!response.ok) return yield* Effect.fail(new Error(`Inngest answered ${response.status} for run ${runId}.`));
    const body = yield* Effect.tryPromise(() => response.json()).pipe(Effect.flatMap(Schema.decodeUnknownEffect(RunStatusBody)));
    return runStatusOf(body.data.status);
  });
