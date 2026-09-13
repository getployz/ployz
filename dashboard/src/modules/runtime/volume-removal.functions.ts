import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import {
  ConfirmVolumeRemoveInput,
  RetryVolumeRemoveInput,
  VolumeResourceInput,
} from "#/modules/runtime/volume-removal";
import {
  confirmVolumeRemove,
  loadLatestVolumeRemoveAttempt,
  loadVolumeRemoveDataLoss,
  retryVolumeRemove,
} from "#/modules/runtime/volume-removal.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

export const loadVolumeRemoveDataLossServerFn = createServerFn({
  method: "GET",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeResourceInput))
  .handler(({ context, data }) =>
    runActor(
      context,
      Effect.scoped(loadVolumeRemoveDataLoss(context.actor, data)),
    ),
  );

export const loadLatestVolumeRemoveAttemptServerFn = createServerFn({
  method: "GET",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(VolumeResourceInput))
  .handler(({ context, data }) =>
    runActor(context, loadLatestVolumeRemoveAttempt(context.actor, data)),
  );

export const confirmVolumeRemoveServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ConfirmVolumeRemoveInput))
  .handler(({ context, data }) =>
    runActor(context, confirmVolumeRemove(context.actor, data)),
  );

export const retryVolumeRemoveServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RetryVolumeRemoveInput))
  .handler(({ context, data }) =>
    runActor(context, retryVolumeRemove(context.actor, data)),
  );
