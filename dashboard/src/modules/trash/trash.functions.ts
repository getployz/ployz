import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { rustMachineIdSchema } from "#/lib/schema";
import { emptyServerTrash, listServerTrash, restoreServerTrash } from "#/modules/trash/trash.server";
import { authenticated, OrganizationSlugInput, runActor, strictValidator } from "#/server/tanstack";

const ServerInput = Schema.Struct({ ...OrganizationSlugInput.fields, machineId: rustMachineIdSchema });
/** The passkey-answered approval as JSON text; the Server parses and checks all of it before it acts. */
const ApprovedInput = Schema.Struct({ ...ServerInput.fields, approval: Schema.String.check(Schema.isMaxLength(64_000)) });

export const listServerTrashServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(ServerInput))
  .handler(({ context, data }) => runActor(context, listServerTrash(context.actor, data)));

export const restoreServerTrashServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(ApprovedInput))
  .handler(({ context, data }) => runActor(context, restoreServerTrash(context.actor, data)));

export const emptyServerTrashServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(ApprovedInput))
  .handler(({ context, data }) => runActor(context, emptyServerTrash(context.actor, data)));
