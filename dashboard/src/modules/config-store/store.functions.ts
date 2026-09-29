import { setResponseHeader } from "@tanstack/react-start/server";
import { createServerFn } from "@tanstack/react-start";
import type { ConfigCommand, ConfigQuery, ConfigView, ConfigWritten } from "@ployz/sdk";
import { callStoreAsMember } from "./config-store.server";
import { storeReadInput, storeWriteInput, type StoreResult } from "./store.contract";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

/** One bounded Store view. Read it through `store-view.queries.ts`, never directly. */
export const readStoreViewServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(storeReadInput))
  .handler(async ({ context, data }): Promise<StoreResult<ConfigView>> => {
    setResponseHeader("cache-control", "private, no-store");
    // SAFETY: the Store decodes and validates the query itself, and answers a read with a view.
    return await runActor(context, callStoreAsMember(context.actor, data.organizationSlug, { operation: "read", query: data.query as ConfigQuery })) as StoreResult<ConfigView>;
  });

/** One Store command, committed or refused. Write through `store-write.ts`, never directly. */
export const writeStoreServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(storeWriteInput))
  .handler(async ({ context, data }): Promise<StoreResult<ConfigWritten>> =>
    // SAFETY: the Store decodes and validates the command itself, and answers a write with what it wrote.
    await runActor(context, callStoreAsMember(context.actor, data.organizationSlug, { operation: "write", command: data.command as ConfigCommand })) as StoreResult<ConfigWritten>);
