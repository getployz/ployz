import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { clusterSealingKey } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";

/** The Sealing Key Cloud recorded for a Cluster pairing, a leaf no Runtime module sits under. */
export const readSealingPublicKey = Effect.fn("SealingKey.readPublicKey")(function* (organizationId: string, clusterKey: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ publicKey: clusterSealingKey.publicKey }).from(clusterSealingKey)
    .where(and(eq(clusterSealingKey.organizationId, organizationId), eq(clusterSealingKey.clusterKey, clusterKey)));
  return row?.publicKey;
});

/**
 * The key new values of a Cluster pairing seal to: the recipient key a Server reported once a person named one at
 * activation, or none before. Values still bind to the Cluster's identity, its first Sealing Key.
 */
export const readSealingRecipient = Effect.fn("SealingKey.readRecipient")(function* (organizationId: string, clusterKey: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ recipient: clusterSealingKey.recipientPublicKey }).from(clusterSealingKey)
    .where(and(eq(clusterSealingKey.organizationId, organizationId), eq(clusterSealingKey.clusterKey, clusterKey)));
  return row?.recipient ?? null;
});

/**
 * Keep `recipient`, which a Server holding `identity` reported, as the key the pairing's new values seal to. Only a
 * pairing recorded under that identity takes it, and a kept one never goes back to none.
 */
export const recordSealingRecipient = Effect.fn("SealingKey.recordRecipient")(function* (
  organizationId: string, clusterKey: string, identity: string, recipient: string,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(clusterSealingKey).set({ recipientPublicKey: recipient })
    .where(and(
      eq(clusterSealingKey.organizationId, organizationId),
      eq(clusterSealingKey.clusterKey, clusterKey),
      eq(clusterSealingKey.publicKey, identity),
    ));
});
