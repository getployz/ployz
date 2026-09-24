import { createServerFn } from "@tanstack/react-start";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";
import {
  bulkUpdateServiceVariables,
  createServiceVariable,
  deleteServiceVariable,
  updateServiceVariable,
  updateServiceVariableExport,
} from "./variable-operations.server";
import {
  bulkUpdateServiceVariablesSchema,
  createServiceVariableSchema,
  deleteServiceVariableSchema,
  updateServiceVariableExportSchema,
  updateServiceVariableSchema,
} from "./variables";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const createServiceVariableServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(createServiceVariableSchema))
  .handler(({ context, data }) =>
    runActor(context, createServiceVariable(context.actor, data)),
  );

export const updateServiceVariableServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(updateServiceVariableSchema))
  .handler(({ context, data }) =>
    runActor(context, updateServiceVariable(context.actor, data)),
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

export const bulkUpdateServiceVariablesServerFn = createServerFn({
  method: "POST",
})
  .middleware(middleware)
  .validator(strictValidator(bulkUpdateServiceVariablesSchema))
  .handler(({ context, data }) =>
    runActor(context, bulkUpdateServiceVariables(context.actor, data)),
  );
