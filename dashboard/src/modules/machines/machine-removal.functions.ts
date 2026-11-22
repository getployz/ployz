import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import {
  EnqueueMachineRemoveInput,
  ForgetServerInput,
  GetMachineRemoveAttemptInput,
  LoadMachineDataLossInput,
} from "#/modules/machines/machine-removal";
import {
  enqueueMachineRemove,
  getMachineRemoveAttempt,
  loadMachineDataLoss,
} from "#/modules/machines/machine-removal.server";
import { forgetServerFromDashboard } from "#/modules/operations/operations.server";
import {
  authenticated,
  callerMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

export const loadMachineDataLossServerFn = createServerFn({
  method: "GET",
})
  .middleware(authenticated)
  .validator(strictValidator(LoadMachineDataLossInput))
  .handler(({ context, data }) =>
    runActor(context, Effect.scoped(loadMachineDataLoss(context.actor, data))),
  );

export const enqueueMachineRemoveServerFn = createServerFn({
  method: "POST",
})
  .middleware(authenticated)
  .validator(strictValidator(EnqueueMachineRemoveInput))
  .handler(({ context, data }) =>
    runActor(context, enqueueMachineRemove(context.actor, data)),
  );

export const getMachineRemoveAttemptServerFn = createServerFn({
  method: "GET",
})
  .middleware(authenticated)
  .validator(strictValidator(GetMachineRemoveAttemptInput))
  .handler(({ context, data }) =>
    runActor(context, getMachineRemoveAttempt(context.actor, data)),
  );

/** Forgets an offline Server: the removal it started. */
export const forgetServerServerFn = createServerFn({
  method: "POST",
})
  .middleware([callerMiddleware(ForgetServerInput)])
  .handler(({ context, data }) =>
    runActor(context, forgetServerFromDashboard(context.caller, data.machineId, data.requestId)),
  );
