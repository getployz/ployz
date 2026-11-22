import { createServerFn } from "@tanstack/react-start";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";
import { updateCanvasPositionSchema } from "./canvas-positions";
import { updateCanvasPosition } from "./canvas-positions.server";

export const updateCanvasPositionServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(updateCanvasPositionSchema))
  .handler(({ context, data }) => runActor(context, updateCanvasPosition(context.actor, data)));
