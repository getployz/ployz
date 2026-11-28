import { createServerFn } from "@tanstack/react-start";
import { Effect, Schema } from "effect";
import { StrictOrganizationSlug } from "#/lib/schema";
import {
  MintMachineEnrollmentInput,
  ResetPendingEnrollmentInput,
} from "#/modules/machines/enrollment";
import {
  mintMachineEnrollment,
  readClusterFounded,
  resetPendingOrganizationEnrollment,
} from "#/modules/machines/enrollment.server";
import { ployzVersion } from "#/modules/runtime/ployz.server";
import { AppConfig } from "#/server/config.server";
import {
  authenticated,
  runActor,
  strictValidator,
} from "#/server/tanstack";

export const resetPendingOrganizationEnrollmentServerFn = createServerFn({
  method: "POST",
})
  .middleware(authenticated)
  .validator(strictValidator(ResetPendingEnrollmentInput))
  .handler(({ context, data }) =>
    runActor(context, resetPendingOrganizationEnrollment(context.actor, data)),
  );

/** One Server's install line: a token shown once, and the release and Cloud the line names. */
export const mintMachineEnrollmentServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(MintMachineEnrollmentInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const minted = yield* mintMachineEnrollment(context.actor, data);
    return { ...minted, cloudOrigin: (yield* AppConfig).app.url.origin, ployzVersion: ployzVersion() };
  })));

export const clusterFoundedServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ organizationSlug: StrictOrganizationSlug })))
  .handler(({ context, data }) => runActor(context, readClusterFounded(context.actor, data)));
