import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { restoreWorkingDocumentSchema } from "./working-document-restore";
import { restoreWorkingDocument } from "./working-document-restore.server";

export const restoreWorkingDocumentServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(restoreWorkingDocumentSchema))
  .handler(({ context, data }) => runActor(context, restoreWorkingDocument(context.actor, data)));
