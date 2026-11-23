import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { prepareDeployTap } from "#/modules/deploy-approval/deploy-tap.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

const PrepareInput = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isTrimmed(), Schema.isNonEmpty()),
  tags: Schema.Array(Schema.String.check(Schema.isMaxLength(512))).check(Schema.isMaxLength(64)),
});

/** One step of a Deploy the person approves: who enforces, which passkeys they trust, and each tag's digest. */
export const prepareDeployTapServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(PrepareInput))
  .handler(({ context, data }) => runActor(context, prepareDeployTap(context.actor, data)));
