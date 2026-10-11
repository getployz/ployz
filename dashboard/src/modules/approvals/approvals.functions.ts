import { createServerFn } from "@tanstack/react-start";
import { SetOrganizationSettingsInput } from "#/modules/approvals/approvals";
import { setOrganizationSettings } from "#/modules/approvals/approvals.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const setOrganizationSettingsServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(SetOrganizationSettingsInput))
  .handler(({ context, data }) => runActor(context, setOrganizationSettings(context.actor, data)));
