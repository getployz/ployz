import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import {
  EnqueueMachineRemoveInput,
  GetMachineRemoveAttemptInput,
  LoadMachineDataLossInput,
} from "#/modules/machines/machine-removal";
import {
  enqueueMachineRemove,
  getMachineRemoveAttempt,
  loadMachineDataLoss,
} from "#/modules/machines/machine-removal.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

export const loadMachineDataLossServerFn = createServerFn({
  method: "GET",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(LoadMachineDataLossInput))
  .handler(({ context, data }) =>
    runActor(context, Effect.scoped(loadMachineDataLoss(context.actor, data))),
  );

export const enqueueMachineRemoveServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(EnqueueMachineRemoveInput))
  .handler(({ context, data }) =>
    runActor(context, enqueueMachineRemove(context.actor, data)),
  );

export const getMachineRemoveAttemptServerFn = createServerFn({
  method: "GET",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(GetMachineRemoveAttemptInput))
  .handler(({ context, data }) =>
    runActor(context, getMachineRemoveAttempt(context.actor, data)),
  );
