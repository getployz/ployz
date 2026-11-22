import { createServerFn } from "@tanstack/react-start";
import {
  ResetPendingEnrollmentInput,
} from "#/modules/machines/enrollment";
import {
  resetPendingOrganizationEnrollment,
} from "#/modules/machines/enrollment.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

export const resetPendingOrganizationEnrollmentServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ResetPendingEnrollmentInput))
  .handler(({ context, data }) =>
    runActor(context, resetPendingOrganizationEnrollment(context.actor, data)),
  );
