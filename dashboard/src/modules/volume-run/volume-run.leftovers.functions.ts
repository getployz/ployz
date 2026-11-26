import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { listServerLeftovers } from "#/modules/volume-run/volume-run.leftovers.server";
import { authenticated, OrganizationSlugInput, runActor, strictValidator } from "#/server/tanstack";

export const listServerLeftoversServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ ...OrganizationSlugInput.fields, machineId: rustMachineIdSchema })))
  .handler(({ context, data }) => runActor(context, listServerLeftovers(context.actor, data)));
