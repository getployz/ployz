import "@tanstack/react-start/server-only";
import type { MachineId, SealingPublicKey } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { clusterSealingKey, clusterSealingKeyGenerator } from "#/modules/machines/tables";
import { Database } from "#/server/database.server";

const sealingKeyOf = (organizationId: string, clusterKey: string) => and(
  eq(clusterSealingKey.organizationId, organizationId),
  eq(clusterSealingKey.clusterKey, clusterKey),
);

const generatorOf = (organizationId: string, clusterKey: string) => and(
  eq(clusterSealingKeyGenerator.organizationId, organizationId),
  eq(clusterSealingKeyGenerator.clusterKey, clusterKey),
);

/** The Sealing public key Cloud kept for this pairing generation, if any Machine has reported one. */
export const readSealingPublicKey = Effect.fn("SealingKey.readPublicKey")(function* (organizationId: string, clusterKey: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ publicKey: clusterSealingKey.publicKey }).from(clusterSealingKey)
    .where(sealingKeyOf(organizationId, clusterKey));
  return row?.publicKey;
});

/**
 * Keeps the first Sealing public key reported for this pairing generation. A later different key is an error to surface,
 * never a replacement.
 */
export const recordSealingPublicKey = Effect.fn("SealingKey.recordPublicKey")(function* (
  organizationId: string, clusterKey: string, publicKey: SealingPublicKey,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(clusterSealingKey).values({ organizationId, clusterKey, publicKey }).onConflictDoNothing();
  const recorded = yield* readSealingPublicKey(organizationId, clusterKey);
  if (recorded !== undefined && recorded !== publicKey) {
    yield* Effect.logError("A Server reported a Sealing Key unlike the one its Cluster first reported.").pipe(
      Effect.annotateLogs({ organizationId, recorded, reported: publicKey }),
    );
  }
});

/** The Machine chosen to generate this pairing generation's Sealing Key, if one was. */
export const readSealingKeyGenerator = Effect.fn("SealingKey.readGenerator")(function* (organizationId: string, clusterKey: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ machineId: clusterSealingKeyGenerator.machineId }).from(clusterSealingKeyGenerator)
    .where(generatorOf(organizationId, clusterKey));
  return row?.machineId;
});

/** Chooses `candidate` to generate the Sealing Key unless a generator was already chosen, and returns the one that stands. */
export const chooseSealingKeyGenerator = Effect.fn("SealingKey.chooseGenerator")(function* (
  organizationId: string, clusterKey: string, candidate: MachineId,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(clusterSealingKeyGenerator).values({ organizationId, clusterKey, machineId: candidate }).onConflictDoNothing();
  return (yield* readSealingKeyGenerator(organizationId, clusterKey)) ?? candidate;
});

/** Forgets the chosen generator once its Machine left the Cluster, so the next backfill names a new candidate. */
export const forgetSealingKeyGenerator = Effect.fn("SealingKey.forgetGenerator")(function* (
  organizationId: string, clusterKey: string, generator: MachineId,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(clusterSealingKeyGenerator)
    .where(and(generatorOf(organizationId, clusterKey), eq(clusterSealingKeyGenerator.machineId, generator)));
});
