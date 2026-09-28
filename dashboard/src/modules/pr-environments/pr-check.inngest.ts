import { Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { prCheckRequestedEventType } from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";
import { postPrCheck } from "./pr-check.server";

type EffectRunner = typeof runInngestEffect;

const PrCheckRequestedData = Schema.Struct({ repositoryId: Schema.Number, number: Schema.Number });

export async function executePostPrCheck(
  { event, step }: { event: { data: unknown }; step: Pick<PloyzStepTools, "run"> },
  runEffect: EffectRunner,
) {
  const decoded = Schema.decodeUnknownOption(PrCheckRequestedData)(event.data, { onExcessProperty: "preserve" });
  if (Option.isNone(decoded)) return { posted: "skipped" as const };
  return { posted: await step.run("post-pr-check", () => runEffect(postPrCheck({ repositoryId: decoded.value.repositoryId, number: decoded.value.number }))) };
}

/**
 * One pull request's posts run one at a time, each from the state as it is then, so an older state never lands last;
 * every project's PR Environment for it shares the one check.
 */
export const createPostPrCheck = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "post-pr-check",
      retries: 3,
      triggers: [{ event: prCheckRequestedEventType }],
      concurrency: [{ key: "event.data.pullRequestKey", limit: 1 }],
    },
    async ({ event, step }) => executePostPrCheck({ event, step }, runEffect),
  );
