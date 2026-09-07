import { createServerFn } from "@tanstack/react-start";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";
import {
  clearServiceRegistryCredential,
  createService,
  deleteServices,
  restoreServiceRegistryCredential,
  setServiceRegistryCredential,
  updateService,
  updateServiceCanvasPosition,
} from "./service-operations.server";
import {
  clearServiceRegistryCredentialSchema,
  createServiceSchema,
  deleteServicesSchema,
  restoreServiceRegistryCredentialSchema,
  setServiceRegistryCredentialSchema,
  updateServiceCanvasPositionSchema,
  updateServiceSchema,
} from "./services";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const createServiceServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(createServiceSchema))
  .handler(({ context, data }) =>
    runActor(context, createService(context.actor, data)),
  );

export const updateServiceServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(updateServiceSchema))
  .handler(({ context, data }) =>
    runActor(context, updateService(context.actor, data)),
  );

export const deleteServicesServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(deleteServicesSchema))
  .handler(({ context, data }) =>
    runActor(context, deleteServices(context.actor, data)),
  );

export const updateServiceCanvasPositionServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(updateServiceCanvasPositionSchema))
  .handler(({ context, data }) =>
    runActor(context, updateServiceCanvasPosition(context.actor, data)),
  );

export const setServiceRegistryCredentialServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(setServiceRegistryCredentialSchema))
  .handler(({ context, data }) =>
    runActor(context, setServiceRegistryCredential(context.actor, data)),
  );

export const clearServiceRegistryCredentialServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(clearServiceRegistryCredentialSchema))
  .handler(({ context, data }) =>
    runActor(context, clearServiceRegistryCredential(context.actor, data)),
  );

export const restoreServiceRegistryCredentialServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(restoreServiceRegistryCredentialSchema))
  .handler(({ context, data }) =>
    runActor(context, restoreServiceRegistryCredential(context.actor, data)),
  );
