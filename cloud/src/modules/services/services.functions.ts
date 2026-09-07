import { createServerFn } from "@tanstack/react-start";
import { restoreServiceWorkingIntentSchema } from "#/modules/environment-design/services";
import { restoreServiceWorkingIntent } from "#/modules/services/service-working-intent.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

// Deployment working-intent restore remains owned by ticket #330.
export const restoreServiceWorkingIntentServerFn = createServerFn({
  method: "POST",
})
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(restoreServiceWorkingIntentSchema))
  .handler(({ context, data }) =>
    runActor(context, restoreServiceWorkingIntent(context.actor, data)),
  );
