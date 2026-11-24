import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { RequestServerDrainInput } from "#/modules/machines/server-drain";
import { listLatestServerDrains, previewServerDrain, requestServerDrain } from "#/modules/machines/server-drain.server";
import { authenticated, callerMiddleware, OrganizationSlugInput, runActor, strictValidator } from "#/server/tanstack";

export const listLatestServerDrainsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationSlugInput))
  .handler(({ context, data }) => runActor(context, listLatestServerDrains(context.actor, data)));

export const previewServerDrainServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ ...OrganizationSlugInput.fields, machineId: rustMachineIdSchema })))
  .handler(({ context, data }) => runActor(context, previewServerDrain(context.actor, data)));

export const requestServerDrainServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(RequestServerDrainInput)])
  .handler(({ context, data }) => runActor(context, requestServerDrain(context.caller, data)));
