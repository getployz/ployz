import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { updateCanvasPositionSchema } from "./canvas-positions";
import { updateCanvasPosition } from "./canvas-positions.server";

export const updateCanvasPositionServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(updateCanvasPositionSchema))
  .handler(({ context, data }) => runActor(context, updateCanvasPosition(context.actor, data)));
