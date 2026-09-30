import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import type { Connection, MachineId } from "@ployz/sdk";
import { and, asc, count, desc, eq } from "drizzle-orm";
import { Data, Effect } from "effect";
import type { EncryptedSecretValue } from "#/db/tables";
import { organizationMachine } from "#/modules/machines/tables";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

export class PairingSecretDecryptFailure extends Data.TaggedError(
  "PairingSecretDecryptFailure",
)<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

export const decryptPairingSecret = Effect.fn("MachineConnections.decryptPairingSecret")(
  function* (value: EncryptedSecretValue) {
    const encryption = yield* SecretEncryption;
    return yield* Effect.try({
      try: () => encryption.decrypt(value),
      catch: (cause) => new PairingSecretDecryptFailure({ cause }),
    });
  },
);

/** How long Cloud waits for a Server to answer: a join to the Organization's Cluster, or each Server Forget Servers tries. */
export const SERVER_REACH_TIMEOUT_MS = 10_000;

/**
 * A founding claim younger than this is a Server still joining: joiners wait for it, and Forget Servers doesn't take its
 * pairing away. An older one without a founder doesn't expire; it only stops making others wait.
 */
const FOUNDING_CLAIM_FRESH_MS = 10 * 60_000;

/** Whether the pairing is a founding claim a Server may still be completing. */
export function foundingClaimFresh(pairing: { founderMachineId: string | null; createdAt: Date }, now = new Date()) {
  return pairing.founderMachineId === null && now.getTime() - pairing.createdAt.getTime() < FOUNDING_CLAIM_FRESH_MS;
}

/** The current pairing's generation, which its Machines carry as their cluster key; none while unpaired or being removed. */
const currentGeneration = Effect.fn("MachineConnections.generation")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select({
    encryptedPairingSecret: organizationPairing.encryptedPairingSecret,
    removalStartedAt: organizationPairing.removalStartedAt,
  }).from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId)).limit(1);
  if (!pairing || pairing.removalStartedAt !== null) return null;
  const secret = yield* decryptPairingSecret(pairing.encryptedPairingSecret);
  return crypto.createHash("sha256").update(secret).digest("hex");
});

/** How many Servers the current pairing has: the Machines `loadOrganizationConnections` would dial, counted. */
export const countOrganizationMachines = Effect.fn("MachineConnections.count")(function* (organizationId: string) {
  const generation = yield* currentGeneration(organizationId);
  if (generation === null) return 0;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ servers: count() }).from(organizationMachine).where(and(
    eq(organizationMachine.organizationId, organizationId),
    eq(organizationMachine.clusterKey, generation),
  ));
  return row?.servers ?? 0;
});

/** Protected candidates are scoped to the current pairing and never browser projections. */
export const loadOrganizationConnections = Effect.fn("MachineConnections.load")(
  function* (organizationId: string) {
    const { drizzle } = yield* Database;
    const generation = yield* currentGeneration(organizationId);
    if (generation === null) return { kind: "missing" as const };
    const candidates = yield* drizzle.select().from(organizationMachine).where(and(
      eq(organizationMachine.organizationId, organizationId),
      eq(organizationMachine.clusterKey, generation),
    )).orderBy(desc(organizationMachine.isDialEntry), asc(organizationMachine.createdAt), asc(organizationMachine.machineId));
    const connections: Connection[] = yield* Effect.forEach(candidates, (candidate) => Effect.gen(function* () {
      const management = yield* decryptPairingSecret(candidate.encryptedCapability);
      // SAFETY: the table constraint enforces the SDK Machine ID representation.
      return { management, machine_id: candidate.machineId as MachineId };
    }));
    return { kind: "ready" as const, generation, connections };
  },
);
