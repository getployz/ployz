import { createServerFn } from "@tanstack/react-start";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";
import { placeCanvasNodesSchema, updateCanvasPositionSchema } from "./canvas-positions";
import { placeCanvasNodes, updateCanvasPosition } from "./canvas-positions.server";

export const updateCanvasPositionServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(updateCanvasPositionSchema))
  .handler(({ context, data }) => runActor(context, updateCanvasPosition(context.actor, data)));

export const placeCanvasNodesServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(placeCanvasNodesSchema))
  .handler(({ context, data }) => runActor(context, placeCanvasNodes(context.actor, data)));
