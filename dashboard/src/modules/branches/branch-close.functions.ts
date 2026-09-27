import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { setBranchKept } from "./branch-close.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const SetBranchKept = Schema.Struct({
  organizationSlug: Schema.String.check(Schema.isNonEmpty()),
  environmentId: Schema.String.check(Schema.isUUID()),
  kept: Schema.Boolean,
});

export const setBranchKeptServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SetBranchKept))
  .handler(({ context, data }) => runActor(context, setBranchKept(context.actor, data)));
