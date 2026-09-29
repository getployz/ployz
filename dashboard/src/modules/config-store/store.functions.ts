import { setResponseHeader } from "@tanstack/react-start/server";
import { createServerFn } from "@tanstack/react-start";
import { callStoreAsMember } from "./config-store.server";
import { storeReadInput, storeWriteInput } from "./store.contract";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

/** One bounded Store view. Read it through `store-view.queries.ts`, never directly. */
export const readStoreViewServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(storeReadInput))
  .handler(async ({ context, data }) => {
    setResponseHeader("cache-control", "private, no-store");
    return await runActor(context, callStoreAsMember(context.actor, data.organizationSlug, { operation: "read", query: data.query }));
  });

/** One Store command, committed or refused. Write through `store-write.ts`, never directly. */
export const writeStoreServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(storeWriteInput))
  .handler(async ({ context, data }) =>
    await runActor(context, callStoreAsMember(context.actor, data.organizationSlug, { operation: "write", command: data.command })));
