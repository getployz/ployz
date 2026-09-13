import { createServerFn } from "@tanstack/react-start";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";
import {
  attachServiceVariableGroup,
  bulkUpdateServiceVariables,
  createServiceVariable,
  createVariableGroupVariable,
  deleteServiceVariable,
  deleteVariableGroupVariable,
  detachServiceVariableGroup,
  updateServiceVariable,
  updateServiceVariableExport,
  updateVariableGroupVariable,
  updateVariableGroupVariableMetadata,
} from "./variable-operations.server";
import {
  attachServiceVariableGroupSchema,
  bulkUpdateServiceVariablesSchema,
  createServiceVariableSchema,
  createVariableGroupVariableSchema,
  deleteServiceVariableSchema,
  deleteVariableGroupVariableSchema,
  detachServiceVariableGroupSchema,
  updateServiceVariableExportSchema,
  updateServiceVariableSchema,
  updateVariableGroupVariableMetadataSchema,
  updateVariableGroupVariableSchema,
} from "./variables";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const createServiceVariableServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(createServiceVariableSchema))
  .handler(({ context, data }) =>
    runActor(context, createServiceVariable(context.actor, data)),
  );

export const createVariableGroupVariableServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(createVariableGroupVariableSchema))
  .handler(({ context, data }) =>
    runActor(context, createVariableGroupVariable(context.actor, data)),
  );

export const updateServiceVariableServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(updateServiceVariableSchema))
  .handler(({ context, data }) =>
    runActor(context, updateServiceVariable(context.actor, data)),
  );

export const updateVariableGroupVariableServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(updateVariableGroupVariableSchema))
  .handler(({ context, data }) =>
    runActor(context, updateVariableGroupVariable(context.actor, data)),
  );

export const updateVariableGroupVariableMetadataServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(updateVariableGroupVariableMetadataSchema))
  .handler(({ context, data }) =>
    runActor(context, updateVariableGroupVariableMetadata(context.actor, data)),
  );

export const updateServiceVariableExportServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(updateServiceVariableExportSchema))
  .handler(({ context, data }) =>
    runActor(context, updateServiceVariableExport(context.actor, data)),
  );

export const deleteServiceVariableServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(deleteServiceVariableSchema))
  .handler(({ context, data }) =>
    runActor(context, deleteServiceVariable(context.actor, data)),
  );

export const deleteVariableGroupVariableServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(deleteVariableGroupVariableSchema))
  .handler(({ context, data }) =>
    runActor(context, deleteVariableGroupVariable(context.actor, data)),
  );

export const bulkUpdateServiceVariablesServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(bulkUpdateServiceVariablesSchema))
  .handler(({ context, data }) =>
    runActor(context, bulkUpdateServiceVariables(context.actor, data)),
  );

export const attachServiceVariableGroupServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(attachServiceVariableGroupSchema))
  .handler(({ context, data }) =>
    runActor(context, attachServiceVariableGroup(context.actor, data)),
  );

export const detachServiceVariableGroupServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(detachServiceVariableGroupSchema))
  .handler(({ context, data }) =>
    runActor(context, detachServiceVariableGroup(context.actor, data)),
  );
