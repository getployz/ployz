import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import {
  ConfirmTeardownInput,
  RetryTeardownInput,
  TeardownTargetInput,
} from "#/modules/runtime/teardown";
import {
  confirmTeardown,
  loadLatestTeardownAttempt,
  loadTeardownDataLoss,
  retryTeardown,
} from "#/modules/runtime/teardown.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

export const loadTeardownDataLossServerFn = createServerFn({
  method: "GET",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(TeardownTargetInput))
  .handler(({ context, data }) =>
    runActor(context, Effect.scoped(loadTeardownDataLoss(context.actor, data))),
  );

export const loadLatestTeardownAttemptServerFn = createServerFn({
  method: "GET",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(TeardownTargetInput))
  .handler(({ context, data }) =>
    runActor(context, loadLatestTeardownAttempt(context.actor, data)),
  );

export const confirmTeardownServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ConfirmTeardownInput))
  .handler(({ context, data }) =>
    runActor(context, Effect.scoped(confirmTeardown(context.actor, data))),
  );

export const retryTeardownServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RetryTeardownInput))
  .handler(({ context, data }) =>
    runActor(context, retryTeardown(context.actor, data)),
  );
