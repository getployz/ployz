import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect } from "effect";
import { dashboardCaller } from "#/modules/identity/caller.server";
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

/** Forgets an offline Server: the removal it started. */
export const forgetServerServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ForgetServerInput))
  .handler(({ context, data }) =>
    runActor(context, Effect.gen(function* () {
      const caller = yield* dashboardCaller(getRequest().headers, data.organizationSlug);
      return yield* forgetServerFromDashboard(caller, data.machineId, data.requestId);
    })),
  );
