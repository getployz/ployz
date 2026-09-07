import { createServerFn } from "@tanstack/react-start";
import {
  MintMachineEnrollmentInput,
  ResetPendingEnrollmentInput,
} from "#/modules/machines/enrollment";
import {
  loadOrganizationEnrollmentStatus,
  mintMachineEnrollment,
  resetPendingOrganizationEnrollment,
} from "#/modules/machines/enrollment.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

const mintInput = strictValidator(MintMachineEnrollmentInput);

export const mintMachineEnrollmentServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(mintInput)
  .handler(({ context, data }) =>
    runActor(context, mintMachineEnrollment(context.actor, data)),
  );

export const loadOrganizationEnrollmentStatusServerFn = createServerFn({
  method: "GET",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(mintInput)
  .handler(({ context, data }) =>
    runActor(context, loadOrganizationEnrollmentStatus(context.actor, data)),
  );

export const resetPendingOrganizationEnrollmentServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ResetPendingEnrollmentInput))
  .handler(({ context, data }) =>
    runActor(context, resetPendingOrganizationEnrollment(context.actor, data)),
  );
