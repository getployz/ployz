import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { MachineId, SealingPublicKey } from "@ployz/sdk";
import { and, eq, isNull, ne, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { createClusterResealRequestedEvent } from "#/modules/inngest/events";
import { currentFounder, currentGeneration } from "#/modules/machines/connections.server";
import { readSealingPublicKey } from "#/modules/machines/sealing-key-read.server";
import type { CliReadSealingRecoveryInput, CliRotateSealingRecoveryInput } from "#/modules/machines/sealing-key";
import { clusterSealingKey, clusterSealingKeyGenerator, organizationSealingKey, sealingKeyOwner } from "#/modules/machines/tables";
import { enqueueEvent } from "#/modules/operations/outbox.server";
import { requireAccessById } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";
import { Conflict, NotFound } from "#/server/public-error";

const generatorOf = (organizationId: string, clusterKey: string) => and(
  eq(clusterSealingKeyGenerator.organizationId, organizationId),
  eq(clusterSealingKeyGenerator.clusterKey, clusterKey),
);

/** A Server reported a Sealing Key that another Organization's Cluster already recorded. Nothing was recorded. */
export class SealingKeyHeldElsewhere extends Data.TaggedError("SealingKeyHeldElsewhere")<{
  readonly organizationId: string;
}> {}

/**
 * Keeps the first Sealing Key a Cluster reports, and gives it to the Organization unless a backup holds another; the first
 * one asks for its Organization's Legacy secrets to be resealed to it, waiting on that send unless `delivery` detaches it.
 * A key is recorded for one Organization only: the first to record it, which may record it again for a later pairing.
 * An Organization that already records it, as from before keys had one, keeps doing so. Any other Organization's report of
 * it records nothing and fails with {@link SealingKeyHeldElsewhere}.
 */
export const recordSealingPublicKey = Effect.fn("SealingKey.recordPublicKey")(function* (
  organizationId: string, clusterKey: string, publicKey: SealingPublicKey, delivery: "await" | "detach" = "await",
) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    // A pairing keeps its first key: a racing record waits on this row, and one that finds a key already here records nothing.
    const inserted = yield* drizzle.insert(clusterSealingKey).values({ organizationId, clusterKey, publicKey }).onConflictDoNothing()
      .returning({ clusterKey: clusterSealingKey.clusterKey });
    if (inserted.length === 0) {
      // A key recorded while Cloud ran a release without owners gets its owner on the pairing's next report.
      const [recorded] = yield* drizzle.select({ publicKey: clusterSealingKey.publicKey }).from(clusterSealingKey)
        .where(and(eq(clusterSealingKey.organizationId, organizationId), eq(clusterSealingKey.clusterKey, clusterKey)));
      if (recorded !== undefined) yield* keyOwner(recorded.publicKey);
      return;
    }
    const owner = yield* keyOwner(publicKey);
    if (owner?.organizationId !== organizationId && !(yield* recordsKey(organizationId, publicKey, clusterKey))) {
      yield* Effect.logError("A Server reported a Sealing Key another Organization's Cluster recorded; Cloud didn't record it.")
        .pipe(Effect.annotateLogs({ organizationId, recordedBy: owner?.organizationId }));
      return yield* new SealingKeyHeldElsewhere({ organizationId });
    }
    yield* enqueueEvent(createClusterResealRequestedEvent({ organizationId, clusterKey }), delivery);
  }));
  const recorded = yield* readSealingPublicKey(organizationId, clusterKey);
  if (recorded !== undefined && recorded !== publicKey) {
    yield* Effect.logError("A Server reported a Sealing Key unlike the one its Cluster first reported.").pipe(
      Effect.annotateLogs({ organizationId, recorded, reported: publicKey }),
    );
  }
  yield* recordOrganizationSealingKey(organizationId, recorded ?? publicKey);
});

/**
 * Returns the key's owner. A key without one goes to the Organization that recorded it first, counting keys kept after
 * forgetting Servers, as the owners' migration did; a racing first record waits on this row.
 */
const keyOwner = Effect.fn("SealingKey.owner")(function* (publicKey: SealingPublicKey) {
  const { drizzle } = yield* Database;
  const [known] = yield* drizzle.select({ organizationId: sealingKeyOwner.organizationId }).from(sealingKeyOwner)
    .where(eq(sealingKeyOwner.publicKey, publicKey));
  if (known !== undefined) return known;
  const [owner] = yield* drizzle.execute<{ organization_id: string }>(sql`
    insert into ${sealingKeyOwner} (public_key, organization_id)
    select ${publicKey}, organization_id from (
      select organization_id, created_at from ${clusterSealingKey} where public_key = ${publicKey}
      union all
      select organization_id, created_at from ${organizationSealingKey} where public_key = ${publicKey}
    ) as records
    order by created_at, organization_id
    limit 1
    on conflict (public_key) do update set organization_id = ${sealingKeyOwner}.organization_id
    returning organization_id
  `, "objects");
  return owner === undefined ? undefined : { organizationId: owner.organization_id };
});

/**
 * {@link recordSealingPublicKey} for a key `reporter` reported. A pairing's first key comes only from its founder: another
 * Server's report records nothing until the founder's does, and a founder that never reports leaves the pairing keyless.
 */
export const recordReportedSealingPublicKey = Effect.fn("SealingKey.recordReported")(function* (
  organizationId: string, clusterKey: string, publicKey: SealingPublicKey, reporter: MachineId,
  delivery: "await" | "detach" = "await",
) {
  if ((yield* readSealingPublicKey(organizationId, clusterKey)) === undefined) {
    const pairing = yield* currentFounder(organizationId);
    if (pairing?.generation !== clusterKey || pairing.founder !== reporter) {
      return yield* Effect.logWarning("A Server other than its Cluster's first reported the Sealing Key; Cloud waits for the first Server's report.")
        .pipe(Effect.annotateLogs({ organizationId, reporter, founder: pairing?.founder ?? null }));
    }
  }
  yield* recordSealingPublicKey(organizationId, clusterKey, publicKey, delivery);
});

export const recoveryDigest = (backup: string) => createHash("sha256").update(Buffer.from(backup, "base64")).digest("hex");

const organizationKeyOf = (organizationId: string) => eq(organizationSealingKey.organizationId, organizationId);

/** Whether the Organization already records the key, for a pairing other than `clusterKey` or as its own. */
const recordsKey = Effect.fn("SealingKey.recordsKey")(function* (
  organizationId: string, publicKey: SealingPublicKey, clusterKey: string,
) {
  const { drizzle } = yield* Database;
  const [cluster] = yield* drizzle.select({ organizationId: clusterSealingKey.organizationId }).from(clusterSealingKey)
    .where(and(
      eq(clusterSealingKey.organizationId, organizationId), eq(clusterSealingKey.publicKey, publicKey),
      ne(clusterSealingKey.clusterKey, clusterKey),
    )).limit(1);
  if (cluster !== undefined) return true;
  const [kept] = yield* drizzle.select({ organizationId: organizationSealingKey.organizationId }).from(organizationSealingKey)
    .where(and(organizationKeyOf(organizationId), eq(organizationSealingKey.publicKey, publicKey)));
  return kept !== undefined;
});

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
