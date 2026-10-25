import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { MachineId, SealingPublicKey } from "@ployz/sdk";
import { and, eq, isNull, ne } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { createClusterResealRequestedEvent } from "#/modules/inngest/events";
import { currentGeneration } from "#/modules/machines/connections.server";
import type { CliReadSealingRecoveryInput, CliRotateSealingRecoveryInput } from "#/modules/machines/sealing-key";
import { clusterSealingKey, clusterSealingKeyGenerator, organizationSealingKey } from "#/modules/machines/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { requireAccessById } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

const sealingKeyOf = (organizationId: string, clusterKey: string) => and(
  eq(clusterSealingKey.organizationId, organizationId),
  eq(clusterSealingKey.clusterKey, clusterKey),
);

const generatorOf = (organizationId: string, clusterKey: string) => and(
  eq(clusterSealingKeyGenerator.organizationId, organizationId),
  eq(clusterSealingKeyGenerator.clusterKey, clusterKey),
);

export const readSealingPublicKey = Effect.fn("SealingKey.readPublicKey")(function* (organizationId: string, clusterKey: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ publicKey: clusterSealingKey.publicKey }).from(clusterSealingKey)
    .where(sealingKeyOf(organizationId, clusterKey));
  return row?.publicKey;
});

/**
 * Keeps the first Sealing Key a Cluster reports, and gives it to the Organization unless a backup holds another; the first
 * one asks for its Organization's Legacy secrets to be resealed to it.
 */
export const recordSealingPublicKey = Effect.fn("SealingKey.recordPublicKey")(function* (
  organizationId: string, clusterKey: string, publicKey: SealingPublicKey,
) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const inserted = yield* drizzle.insert(clusterSealingKey).values({ organizationId, clusterKey, publicKey }).onConflictDoNothing()
      .returning({ clusterKey: clusterSealingKey.clusterKey });
    if (inserted.length > 0) yield* enqueueEvent(createClusterResealRequestedEvent({ organizationId, clusterKey }));
  }));
  const recorded = yield* readSealingPublicKey(organizationId, clusterKey);
  if (recorded !== undefined && recorded !== publicKey) {
    yield* Effect.logError("A Server reported a Sealing Key unlike the one its Cluster first reported.").pipe(
      Effect.annotateLogs({ organizationId, recorded, reported: publicKey }),
    );
  }
  yield* recordOrganizationSealingKey(organizationId, recorded ?? publicKey);
});

export const recoveryDigest = (backup: string) => createHash("sha256").update(Buffer.from(backup, "base64")).digest("hex");

const organizationKeyOf = (organizationId: string) => eq(organizationSealingKey.organizationId, organizationId);

export const readOrganizationSealingKey = Effect.fn("SealingKey.readOrganizationKey")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ publicKey: organizationSealingKey.publicKey, recoveryBlob: organizationSealingKey.recoveryBlob })
    .from(organizationSealingKey).where(organizationKeyOf(organizationId));
  return row;
});

/**
 * Keeps the Organization's key once a backup holds it. Before then no Server outside the current Cluster can be given
 * that key, so the Cluster's key replaces it.
 */
const recordOrganizationSealingKey = Effect.fn("SealingKey.recordOrganizationKey")(function* (
  organizationId: string, publicKey: SealingPublicKey,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(organizationSealingKey).values({ organizationId, publicKey }).onConflictDoUpdate({
    target: organizationSealingKey.organizationId,
    set: { publicKey, updatedAt: new Date() },
    setWhere: and(isNull(organizationSealingKey.recoveryBlob), ne(organizationSealingKey.publicKey, publicKey)),
  });
  const recorded = (yield* readOrganizationSealingKey(organizationId))?.publicKey;
  if (recorded !== undefined && recorded !== publicKey) {
    yield* Effect.logError("A Server reported a Sealing Key unlike the one its Organization's recovery backup holds.").pipe(
      Effect.annotateLogs({ organizationId, recorded, reported: publicKey }),
    );
  }
});

const backupConflict = () => new Conflict({ message: "The Organization's Sealing Key or its recovery backup has changed." });

export const recordFirstRecoveryBackup = Effect.fn("SealingKey.recordFirstRecoveryBackup")(function* (
  organizationId: string, publicKey: SealingPublicKey, backup: string,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(organizationSealingKey).values({ organizationId, publicKey, recoveryBlob: backup }).onConflictDoNothing();
  yield* drizzle.update(organizationSealingKey).set({ recoveryBlob: backup, updatedAt: new Date() })
    .where(and(organizationKeyOf(organizationId), eq(organizationSealingKey.publicKey, publicKey), isNull(organizationSealingKey.recoveryBlob)));
  const stored = yield* readOrganizationSealingKey(organizationId);
  if (stored?.publicKey !== publicKey || stored.recoveryBlob !== backup) return yield* backupConflict();
  return recoveryDigest(backup);
});

export const readSealingRecovery = Effect.fn("SealingKey.readRecovery")(function* (organizationId: string) {
  const generation = yield* currentGeneration(organizationId);
  const clusterKey = generation === null ? undefined : yield* readSealingPublicKey(organizationId, generation);
  if (clusterKey !== undefined) yield* recordOrganizationSealingKey(organizationId, clusterKey);
  const stored = yield* readOrganizationSealingKey(organizationId);
  if (stored === undefined) return yield* new NotFound({ message: "This Organization has no Sealing Key." });
  return {
    publicKey: stored.publicKey,
    backup: stored.recoveryBlob,
    digest: stored.recoveryBlob === null ? null : recoveryDigest(stored.recoveryBlob),
  };
});

export const rotateRecoveryBackup = Effect.fn("SealingKey.rotateRecoveryBackup")(function* (
  organizationId: string, publicKey: string, currentDigest: string | null, backup: string,
) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [stored] = yield* drizzle.select({ publicKey: organizationSealingKey.publicKey, recoveryBlob: organizationSealingKey.recoveryBlob })
      .from(organizationSealingKey).where(organizationKeyOf(organizationId)).for("update");
    if (stored === undefined) return yield* new NotFound({ message: "This Organization has no Sealing Key." });
    if (stored.publicKey !== publicKey) return yield* backupConflict();
    const digest = recoveryDigest(backup);
    const storedDigest = stored.recoveryBlob === null ? null : recoveryDigest(stored.recoveryBlob);
    if (storedDigest === digest) return { digest };
    if (storedDigest !== currentDigest) return yield* backupConflict();
    yield* drizzle.update(organizationSealingKey).set({ recoveryBlob: backup, updatedAt: new Date() })
      .where(organizationKeyOf(organizationId));
    return { digest };
  }));
});

export const readCliSealingRecovery = Effect.fn("SealingKey.readRecoveryForCli")(function* (
  actor: Actor, input: CliReadSealingRecoveryInput,
) {
  yield* requireAccessById(actor, input.organizationId, "servers:operate");
  return yield* readSealingRecovery(input.organizationId);
});

export const rotateCliSealingRecovery = Effect.fn("SealingKey.rotateRecoveryForCli")(function* (
  actor: Actor, input: CliRotateSealingRecoveryInput,
) {
  yield* requireAccessById(actor, input.organizationId, "servers:operate");
  return yield* rotateRecoveryBackup(input.organizationId, input.publicKey, input.currentDigest, input.backup);
});

export const readSealingKeyGenerator = Effect.fn("SealingKey.readGenerator")(function* (organizationId: string, clusterKey: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ machineId: clusterSealingKeyGenerator.machineId }).from(clusterSealingKeyGenerator)
    .where(generatorOf(organizationId, clusterKey));
  return row?.machineId;
});

export const chooseSealingKeyGenerator = Effect.fn("SealingKey.chooseGenerator")(function* (
  organizationId: string, clusterKey: string, candidate: MachineId,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(clusterSealingKeyGenerator).values({ organizationId, clusterKey, machineId: candidate }).onConflictDoNothing();
  return (yield* readSealingKeyGenerator(organizationId, clusterKey)) ?? candidate;
});

export const forgetSealingKeyGenerator = Effect.fn("SealingKey.forgetGenerator")(function* (
  organizationId: string, clusterKey: string, generator: MachineId,
) {
  const { drizzle } = yield* Database;
  yield* drizzle.delete(clusterSealingKeyGenerator)
    .where(and(generatorOf(organizationId, clusterKey), eq(clusterSealingKeyGenerator.machineId, generator)));
});
