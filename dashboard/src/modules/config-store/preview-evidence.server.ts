import "@tanstack/react-start/server-only";
import type { ConfigTrusted } from "@ployz/sdk";
import { Effect, Option } from "effect";
import { cloudStore, deployDialOf, storeTry, withDeployDial } from "#/modules/config-store/store-sdk.server";
import { githubMembers } from "#/modules/config-store/store-sources.server";
import { observePreviewHomes, observeSigners } from "#/modules/runtime/ployz.server";
import type { StoreRead } from "./store.contract";

/** Whether the Organization has a preview to place or close: a PR plan keeps a preview rule, or a preview is open. */
const usesPreviews = (read: StoreRead) =>
  storeTry(() => read({ query: "open_previews", namespaces: [] })).pipe(
    Effect.map(({ active }) => active),
    Effect.orElseSucceed(() => false),
  );

/**
 * The Servers that can hold a preview, as Cloud reads them from the Cluster, for the Store to place previews on and to
 * close those whose Server left, with the members whose pull requests a preview opens again for. Undefined, so the
 * Store opens and closes none, when the Organization uses no previews or the Cluster can't be read.
 */
export const gatherPreviewHomes = Effect.fn("ConfigStore.previewHomes")(function* (organizationId: string, read: StoreRead) {
  if (!(yield* usesPreviews(read))) return undefined;
  return yield* observeHomes(organizationId);
});

/**
 * The Servers that can hold a preview now, for a preview rule's draft to list: read even before the Organization uses
 * previews, since turning them on picks among them. Undefined when the Cluster can't be read.
 */
export const observeHomes = Effect.fn("ConfigStore.observeHomes")(function* (organizationId: string) {
  const observed = yield* withDeployDial(organizationId, ({ connections, expect }) =>
    observePreviewHomes(connections, expect)).pipe(Effect.option);
  const homes = Option.getOrUndefined(observed)?.homes;
  if (homes === undefined) return undefined;
  return { preview_homes: homes, members: yield* githubMembers(organizationId) } satisfies Pick<ConfigTrusted, "preview_homes" | "members">;
});

/** The keys a Server of the Cluster trusts, which a preview rule's signature must check against. Undefined when none answered. */
export const gatherSigners = Effect.fn("ConfigStore.signers")(function* (organizationId: string) {
  const observed = yield* withDeployDial(organizationId, ({ connections, expect }) =>
    observeSigners(connections, expect)).pipe(Effect.option);
  return Option.getOrUndefined(Option.map(observed, ({ devices, passkeys, grants }) =>
    ({ devices, passkeys, grants }) satisfies NonNullable<ConfigTrusted["signers"]>));
});

/**
 * Close, on each Server, every preview it holds open that the Store doesn't, and finish every Close one began: one whose
 * Close never reached it, or one from before a Server restarted. Each Server's list is read before the Store, so a
 * preview opening meanwhile stays. Every Server is asked, even once no plan keeps previews, since one that was down
 * may still hold some. A Close destroys data, so none goes to a Cluster whose recorded Sealing Key the dial didn't match.
 */
export const closeUnheldPreviews = Effect.fn("ConfigStore.closeUnheldPreviews")(function* (organizationId: string) {
  const store = yield* cloudStore;
  const { connections, expect } = yield* deployDialOf(organizationId);
  if (connections.length === 0) return;
  if (expect === null) {
    yield* Effect.logWarning("Closed no previews: Cloud has no Sealing Key recorded to check which Cluster the Servers are.", { organizationId });
    return;
  }
  const swept = yield* withDeployDial(organizationId, ({ connections, expect }) =>
    store.closeUnheldPreviews(organizationId, connections, expect));
  if (swept.unverified) yield* Effect.logWarning("Closed no previews: Cloud couldn't check that the Servers are this Organization's Cluster.", { organizationId });
  if (swept.closed.length > 0) yield* Effect.logInfo("Closed previews the Store no longer holds open.", { organizationId, closed: swept.closed });
  if (swept.failed.length > 0) yield* Effect.logWarning("A preview's Close failed; the next sweep retries it.", { organizationId, failed: swept.failed });
});
