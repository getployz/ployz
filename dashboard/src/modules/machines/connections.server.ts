import "@tanstack/react-start/server-only";
import crypto from "node:crypto";
import type { Connection, MachineId } from "@ployz/sdk";
import { and, asc, desc, eq } from "drizzle-orm";
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

/** Protected candidates are scoped to the current pairing and never browser projections. */
export const loadOrganizationConnections = Effect.fn("MachineConnections.load")(
  function* (organizationId: string) {
    const { drizzle } = yield* Database;
    const [pairing] = yield* drizzle.select({
      encryptedPairingSecret: organizationPairing.encryptedPairingSecret,
      removalStartedAt: organizationPairing.removalStartedAt,
    }).from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId)).limit(1);
    if (!pairing || pairing.removalStartedAt !== null) return { kind: "missing" as const };
    const secret = yield* decryptPairingSecret(pairing.encryptedPairingSecret);
    const generation = crypto.createHash("sha256").update(secret).digest("hex");
    const candidates = yield* drizzle.select().from(organizationMachine).where(and(
      eq(organizationMachine.organizationId, organizationId),
      eq(organizationMachine.clusterKey, generation),
    )).orderBy(desc(organizationMachine.isDialEntry), asc(organizationMachine.createdAt), asc(organizationMachine.machineId));
    const connections: Connection[] = yield* Effect.forEach(candidates, (candidate) => Effect.gen(function* () {
      const tailcat = yield* decryptPairingSecret(candidate.encryptedTailcat);
      // SAFETY: the table constraint enforces the SDK Machine ID representation.
      return { tailcat, machine_id: candidate.machineId as MachineId };
    }));
    return { kind: "ready" as const, generation, connections };
  },
);
