import { createServerFn } from "@tanstack/react-start";
import {
  discardVolumeResource,
  restoreVariableGroupResourceSnapshot,
} from "#/modules/environment-design/environment-resource-snapshot-actions.server";
import {
  discardVolumeResourceSchema,
  restoreVariableGroupResourceSnapshotSchema,
} from "#/modules/environment-design/resources";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const restoreVariableGroupResourceSnapshotServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(restoreVariableGroupResourceSnapshotSchema))
  .handler(({ context, data }) =>
    runActor(context, restoreVariableGroupResourceSnapshot(context.actor, data)),
  );

export const discardVolumeResourceServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(discardVolumeResourceSchema))
  .handler(({ context, data }) =>
    runActor(context, discardVolumeResource(context.actor, data)),
  );
