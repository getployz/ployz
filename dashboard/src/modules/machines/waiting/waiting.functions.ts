import { createServerFn } from "@tanstack/react-start";
import { OpenProbeSelectionInput } from "#/modules/machines/waiting/probe";
import { openProbeSelection } from "#/modules/machines/waiting/probe.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const openProbeSelectionServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(OpenProbeSelectionInput))
  .handler(({ context, data }) => runActor(context, openProbeSelection(context.actor, data)));
