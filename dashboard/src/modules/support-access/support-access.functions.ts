import { createServerFn } from "@tanstack/react-start";
import { SetSupportAccessInput } from "#/modules/support-access/support-access";
import { setSupportAccess } from "#/modules/support-access/support-access.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const setSupportAccessServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SetSupportAccessInput))
  .handler(({ context, data }) => runActor(context, setSupportAccess(context.actor, data)));
