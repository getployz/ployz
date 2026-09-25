import { DeploymentExecutionError } from "./execution-error";
import type { Client, PreparedDeploy } from "@ployz/sdk";
import { assert, it } from "@effect/vitest";
import { Effect, Result } from "effect";
import { asTestDouble } from "#/lib/test-double";
import { makePloyzLayer, Ployz } from "#/modules/runtime/ployz.server";
import { watchDeploymentCancellation } from "./runtime-activities.server";

const connections = [{ management: "ployz1:candidate" }];

it("aborts a quiet build when cancellation polling fails and awaits cleanup", async () => {
  let begin: () => void = () => undefined;
  let abortObserved: () => void = () => undefined;
  let finish: () => void = () => undefined;
  const started = new Promise<void>((resolve) => { begin = resolve; });
  const aborted = new Promise<void>((resolve) => { abortObserved = resolve; });
  const finished = new Promise<PreparedDeploy>((_resolve, reject) => { finish = () => reject(new Error("Cleanup confirmed")); });
  void finished.catch(() => undefined);
  let closed = false;
  const layer = makePloyzLayer({ connect: async () => asTestDouble<Client>()({
    prepare: () => {
      begin();
      return { finished, abort: () => abortObserved(), async *[Symbol.asyncIterator]() { yield* []; await finished; } };
    },
    close: async () => { closed = true; },
  }) });
  const controller = new AbortController();
  const failedPoll = Effect.promise(() => started).pipe(Effect.andThen(Effect.fail(new Error("Database poll unavailable"))));
  const running = Effect.runPromise(Effect.scoped(Effect.gen(function* () {
    const session = yield* (yield* Ployz).connect({ connections });
    return yield* session.prepare({ deployment: { projectName: "test", snapshots: [] }, sources: {} }, async () => undefined, controller.signal)
      .pipe(Effect.raceFirst(watchDeploymentCancellation(failedPoll, controller)));
  })).pipe(Effect.provide(layer), Effect.result));
  await aborted;
  assert.isTrue(controller.signal.aborted);
  assert.isFalse(closed);
  finish();
  const result = await running;
  assert.isTrue(Result.isFailure(result));
  if (Result.isFailure(result)) {
    assert.instanceOf(result.failure, DeploymentExecutionError);
    if (result.failure instanceof DeploymentExecutionError) assert.strictEqual(result.failure.failureCode, "sdk_deploy_outcome_unknown");
  }
  assert.isTrue(closed);
});
