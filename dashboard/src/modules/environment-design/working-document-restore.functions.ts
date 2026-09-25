import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { discardEnvironmentChangesSchema } from "./working-document-restore";
import { discardEnvironmentChanges } from "./saved-state-operations.server";

export const discardEnvironmentChangesServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(discardEnvironmentChangesSchema))
  .handler(({ context, data }) => runActor(context, discardEnvironmentChanges(context.actor, data)));
