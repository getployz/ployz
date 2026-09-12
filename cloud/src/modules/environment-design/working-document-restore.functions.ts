import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { discardEnvironmentChangesSchema, restoreWorkingDocumentSchema } from "./working-document-restore";
import { restoreWorkingDocument } from "./working-document-restore.server";
import { discardEnvironmentChanges } from "./environment-change-set.server";

export const restoreWorkingDocumentServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(restoreWorkingDocumentSchema))
  .handler(({ context, data }) => runActor(context, restoreWorkingDocument(context.actor, data)));

export const discardEnvironmentChangesServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(discardEnvironmentChangesSchema))
  .handler(({ context, data }) => runActor(context, discardEnvironmentChanges(context.actor, data)));
