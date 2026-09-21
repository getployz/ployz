import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { serviceMetadataEditSchema } from "./service-metadata";
import { editServiceMetadata } from "./service-metadata.server";

export const editServiceMetadataServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(serviceMetadataEditSchema))
  .handler(({ context, data }) => runActor(context, editServiceMetadata(context.actor, data)));
