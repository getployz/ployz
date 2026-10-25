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
