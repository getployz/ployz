import "@tanstack/react-start/server-only";
import { observePreviewHomes } from "@ployz/sdk";
import { Effect, Option } from "effect";
import { cloudStore, storeTry, withDeployDial } from "#/modules/config-store/store-sdk.server";
import type { StoreRead } from "./store.contract";

/** Whether the Organization has a preview to place or close: a PR plan keeps a preview rule, or a preview is open. */
const usesPreviews = (read: StoreRead) =>
  storeTry(() => read({ query: "open_previews", namespaces: [] })).pipe(
    Effect.map(({ active }) => active),
    Effect.orElseSucceed(() => false),
  );

/**
 * The Servers that can hold a preview, as Cloud reads them from the Cluster, for the Store to place previews on and to
 * close those whose Server left. Undefined, so the Store opens and closes none, when the Organization uses no previews
 * or the Cluster can't be read.
 */
export const gatherPreviewHomes = Effect.fn("ConfigStore.previewHomes")(function* (organizationId: string, read: StoreRead) {
  if (!(yield* usesPreviews(read))) return undefined;
  const observed = yield* withDeployDial(organizationId, ({ connections, expect }) =>
    observePreviewHomes(connections, expect)).pipe(Effect.option);
  return Option.getOrUndefined(observed)?.homes;
});

/**
 * Close, on each Server, every preview it holds open that the Store doesn't: one whose Close never reached it, or one
 * from before a Server restarted. Each Server's list is read before the Store, so a preview opening meanwhile stays.
 */
export const closeUnheldPreviews = Effect.fn("ConfigStore.closeUnheldPreviews")(function* (organizationId: string) {
  const store = yield* cloudStore;
  if (!(yield* usesPreviews((query) => store.read(organizationId, query)))) return;
  const swept = yield* withDeployDial(organizationId, ({ connections, expect }) =>
    store.closeUnheldPreviews(organizationId, connections, expect));
  if (swept.closed.length > 0) yield* Effect.logInfo("Closed previews the Store no longer holds open.", { organizationId, closed: swept.closed });
  if (swept.failed.length > 0) yield* Effect.logWarning("A preview's Close failed; the next sweep retries it.", { organizationId, failed: swept.failed });
});
