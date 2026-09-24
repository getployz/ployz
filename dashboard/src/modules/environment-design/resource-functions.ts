import { createServerFn } from "@tanstack/react-start";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";
import {
  attachServiceVolume,
  detachServiceVolume,
  updateServiceVolumeMountPath,
} from "./mount-operations.server";
import {
  createVolumeResource,
  deleteVolumeResource,
  updateEnvironmentResourceCanvasPosition,
  updateVolumeResource,
} from "./resource-operations.server";
import {
  createVolumeResourceSchema,
  deleteVolumeResourceSchema,
  updateEnvironmentResourceCanvasPositionSchema,
  updateVolumeResourceSchema,
} from "./resources";
import {
  attachServiceVolumeSchema,
  detachServiceVolumeSchema,
  updateServiceVolumeMountPathSchema,
} from "./service-volume-attachments";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const createVolumeResourceServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(createVolumeResourceSchema))
  .handler(({ context, data }) =>
    runActor(context, createVolumeResource(context.actor, data)),
  );

export const updateVolumeResourceServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(updateVolumeResourceSchema))
  .handler(({ context, data }) =>
    runActor(context, updateVolumeResource(context.actor, data)),
  );

export const deleteVolumeResourceServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(deleteVolumeResourceSchema))
  .handler(({ context, data }) =>
    runActor(context, deleteVolumeResource(context.actor, data)),
  );

export const updateEnvironmentResourceCanvasPositionServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(updateEnvironmentResourceCanvasPositionSchema))
  .handler(({ context, data }) =>
    runActor(context, updateEnvironmentResourceCanvasPosition(context.actor, data)),
  );

export const attachServiceVolumeServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(attachServiceVolumeSchema))
  .handler(({ context, data }) =>
    runActor(context, attachServiceVolume(context.actor, data)),
  );

export const updateServiceVolumeMountPathServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(updateServiceVolumeMountPathSchema))
  .handler(({ context, data }) =>
    runActor(context, updateServiceVolumeMountPath(context.actor, data)),
  );

export const detachServiceVolumeServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(detachServiceVolumeSchema))
  .handler(({ context, data }) =>
    runActor(context, detachServiceVolume(context.actor, data)),
  );
