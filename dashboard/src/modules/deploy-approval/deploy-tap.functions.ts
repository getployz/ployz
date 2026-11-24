import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { EnvironmentRef } from "#/modules/config-store/store.contract";
import { prepareDeployTap, prepareSaveTap } from "#/modules/deploy-approval/deploy-tap.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

const PrepareInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty()),
  environment: EnvironmentRef,
  services: Schema.Array(Schema.String.check(Schema.isMaxLength(256))).check(Schema.isMaxLength(256)),
});

/** A Deploy's plan as Cloud reads it, with who enforces, which passkeys they trust, and each signed tag's digest. */
export const prepareDeployTapServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(PrepareInput))
  .handler(({ context, data: { environment, ...data } }) => runActor(context, prepareDeployTap(context.actor, {
    ...data, environment: { project: environment.project ?? null, environment: environment.environment ?? null },
  })));

const SaveInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty()),
  environment: EnvironmentRef,
});

/** What a Save saves, as the automatic deploy rule reads it, with who enforces and which passkeys they trust. */
export const prepareSaveTapServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(SaveInput))
  .handler(({ context, data: { environment, ...data } }) => runActor(context, prepareSaveTap(context.actor, {
    ...data, environment: { project: environment.project ?? null, environment: environment.environment ?? null },
  })));
