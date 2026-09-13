import { setResponseHeader } from "@tanstack/react-start/server";
import { createServerFn } from "@tanstack/react-start";
import { collectionReadInput } from "./read.contract";
import { readCollection } from "./read.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const readCollectionServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(collectionReadInput))
  .handler(({ context, data }) => {
    setResponseHeader("cache-control", "private, no-store");
    return runActor(context, readCollection(context.actor, data));
  });
