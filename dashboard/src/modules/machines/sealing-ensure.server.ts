import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createSealingKeyEnsureRequestedEvent } from "#/modules/inngest/events";
import { sealingHolders } from "#/modules/machines/sealing-holders";
import { readSealingPublicKey } from "#/modules/machines/sealing-key-read.server";

/**
 * Asks the ensure job to record the key the Servers of `generation` report, or to share it with one that lacks it.
 * Reads the key row through the leaf, since the frame recorder sits under the Organization Runtime.
 */
export const requestSealingKeyEnsure = Effect.fn("SealingKey.requestEnsure")(function* (
  organizationId: string,
  generation: string,
  frame: Pick<RuntimeWatchView, "machines">,
) {
  const { held, lacking } = sealingHolders(frame);
  if (held.length === 0) return "settled";
  // Uninterruptible though its caller times it out, since interrupting pooled SQL can cancel a later statement.
  if (lacking.length === 0 && (yield* Effect.uninterruptible(readSealingPublicKey(organizationId, generation))) !== undefined) return "settled";
  yield* sendInngestEvent(createSealingKeyEnsureRequestedEvent({ organizationId }));
  return "asked";
});
