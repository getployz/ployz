import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Effect } from "effect";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createSealingKeyEnsureRequestedEvent } from "#/modules/inngest/events";
import { currentGeneration } from "#/modules/machines/connections.server";
import { sealingHolders } from "#/modules/machines/sealing-holders";
import { readSealingPublicKey } from "#/modules/machines/sealing-key-read.server";

/** Whether Cloud holds the key the current pairing's Servers report. Reads the leaf, since the frame recorder sits under the Organization Runtime. */
const recorded = Effect.fn("SealingKey.recorded")(function* (organizationId: string) {
  const generation = yield* currentGeneration(organizationId);
  return generation !== null && (yield* readSealingPublicKey(organizationId, generation)) !== undefined;
});

export const requestSealingKeyEnsure = Effect.fn("SealingKey.requestEnsure")(function* (
  organizationId: string,
  frame: Pick<RuntimeWatchView, "machines">,
) {
  const { held, lacking } = sealingHolders(frame);
  if (held.length === 0) return "settled";
  // Uninterruptible though its caller times it out, since interrupting pooled SQL can cancel a later statement.
  if (lacking.length === 0 && (yield* Effect.uninterruptible(recorded(organizationId)))) return "settled";
  yield* sendInngestEvent(createSealingKeyEnsureRequestedEvent({ organizationId }));
  return "asked";
});
