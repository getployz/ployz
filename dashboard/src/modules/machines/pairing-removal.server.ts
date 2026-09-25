import "@tanstack/react-start/server-only";

import { createHash } from "node:crypto";
import type { Connection, MachineId } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Data, Effect, Option, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import {
  enrollmentAllocation,
  machineEnrollmentToken,
  organizationMachine,
} from "#/modules/machines/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Ployz } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

import { RemovalEndpoints, type RemovalEndpoint } from "#/modules/machines/pairing-removal";

type Pairing = typeof organizationPairing.$inferSelect;
type RemovalAttempt = Pairing & {
  removalStartedAt: Date;
  removalEndpoints: readonly RemovalEndpoint[];
};

export class PairingRemovalStateInvalid extends Data.TaggedError("PairingRemovalStateInvalid") {
  readonly publicErrorCategory = "internal" as const;
}

const decodeEndpoints = Effect.fn("PairingRemoval.decodeEndpoints")(
  Schema.decodeUnknownEffect(RemovalEndpoints, { onExcessProperty: "error" }),
  Effect.mapError(() => new PairingRemovalStateInvalid()),
);

const decrypt = Effect.fn("PairingRemoval.decrypt")(function* (
  value: Parameters<SecretEncryption["Service"]["decrypt"]>[0],
) {
  const encryption = yield* SecretEncryption;
  return yield* Effect.try({
    try: () => encryption.decrypt(value),
    catch: () => new Conflict({ message: "The protected removal credential could not be read." }),
  });
});

/** Move usable credentials out of ordinary lookup before doing any remote work. */
export const disableOrganizationPairing = Effect.fn("PairingRemoval.disable")(
  function* (organizationId: string) {
    const database = yield* Database;
    const disabled = yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [pairing] = yield* drizzle.select().from(organizationPairing)
        .where(eq(organizationPairing.organizationId, organizationId)).for("update");
      if (!pairing) return null;
      const secret = yield* decrypt(pairing.encryptedPairingSecret);
      const generation = createHash("sha256").update(secret).digest("hex");
      let attempt: RemovalAttempt;
      if (pairing.removalStartedAt !== null && pairing.removalEndpoints !== null) {
        attempt = { ...pairing, removalStartedAt: pairing.removalStartedAt, removalEndpoints: yield* decodeEndpoints(pairing.removalEndpoints) };
      } else {
        const candidates = yield* drizzle.select().from(organizationMachine)
          .where(eq(organizationMachine.organizationId, organizationId));
        const [allocation] = yield* drizzle.select().from(enrollmentAllocation).where(and(
          eq(enrollmentAllocation.organizationId, organizationId),
          eq(enrollmentAllocation.clusterKey, generation),
        ));
        const removalEndpoints = [...(yield* decodeEndpoints(candidates.map((candidate) => ({
          machineId: candidate.machineId,
          encryptedExpected: candidate.encryptedCapability,
          status: "pending",
        }))))];
        // A claim or reserved Join may have reached a Machine before publication was acknowledged.
        const intendedMachines = [pairing.founderClaimMachineId, ...(allocation?.assignments.map((assignment) => assignment.machine.id) ?? [])];
        for (const machineId of intendedMachines) {
          if (!removalEndpoints.some((endpoint) => endpoint.machineId === machineId)) {
            removalEndpoints.push({ machineId: yield* Schema.decodeUnknownEffect(rustMachineIdSchema)(machineId), status: "unknown" });
          }
        }
        const removalStartedAt = new Date();
        yield* drizzle.update(organizationPairing).set({ removalStartedAt, removalEndpoints })
          .where(eq(organizationPairing.organizationId, organizationId));
        // This also retires the old generation's preferred-entry flag.
        yield* drizzle.delete(organizationMachine).where(eq(organizationMachine.organizationId, organizationId));
        yield* drizzle.delete(machineEnrollmentToken).where(eq(machineEnrollmentToken.organizationId, organizationId));
        attempt = { ...pairing, removalStartedAt, removalEndpoints };
      }
      return { attempt, generation };
    }));
    if (disabled !== null) {
      const runtime = yield* OrganizationRuntime;
      yield* runtime.cancel(organizationId, disabled.generation);
    }
    return disabled?.attempt ?? null;
  },
);

const loadCurrentAttempt = Effect.fn("PairingRemoval.loadCurrent")(
  function* (attempt: RemovalAttempt) {
    const { drizzle } = yield* Database;
    const [current] = yield* drizzle.select().from(organizationPairing).where(and(
      eq(organizationPairing.organizationId, attempt.organizationId),
      eq(organizationPairing.removalStartedAt, attempt.removalStartedAt),
    )).for("update");
    if (current?.removalEndpoints === null || current?.removalEndpoints === undefined) {
      return yield* new Conflict({ message: "The pairing removal attempt is no longer current." });
    }
    return { ...current, removalEndpoints: yield* decodeEndpoints(current.removalEndpoints) };
  },
);

const PairingCleared = Schema.Struct({
  code: Schema.Literal("unauthenticated"),
  details: Schema.Struct({ management_pairing: Schema.Literal("cleared") }),
});

/** Clear one Machine's `cloud` Management Client. Only a successful Clear or an authenticated cleared response confirms removal. */
const removeEndpointPairing = Effect.fn("PairingRemoval.removeEndpoint")(
  function* (machineId: MachineId, management: string) {
    const ployz = yield* Ployz;
    return yield* Effect.scoped(Effect.gen(function* () {
      const session = yield* ployz.connect({ connections: [{ machine_id: machineId, management }], timeoutMs: 10_000 });
      yield* session.clearManagementClient("cloud");
      return true;
    })).pipe(Effect.catch((error) => Effect.succeed(
      error._tag === "PloyzProviderError" && error.operation === "connect"
        && Option.isSome(Schema.decodeUnknownOption(PairingCleared)(error.cause)),
    )));
  },
);

/** Endpoint failures are unconfirmed outcomes, never evidence that old access was revoked. */
export const revokeOrganizationPairing = Effect.fn("PairingRemoval.revoke")(
  function* (organizationId: string) {
    const attempt = yield* disableOrganizationPairing(organizationId);
    const database = yield* Database;
    if (attempt === null) {
      const candidates = yield* database.drizzle.select({ machineId: organizationMachine.machineId })
        .from(organizationMachine).where(eq(organizationMachine.organizationId, organizationId));
      return { confirmed: candidates.length === 0, endpoints: candidates.map(({ machineId }) => ({ machineId, status: "unconfirmed" as const })) };
    }
    yield* Effect.forEach(attempt.removalEndpoints, (endpoint) => Effect.gen(function* () {
      if (endpoint.status !== "pending") return;
      const management = yield* decrypt(endpoint.encryptedExpected);
      if (!(yield* removeEndpointPairing(endpoint.machineId, management))) return;
      yield* database.transaction(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const current = yield* loadCurrentAttempt(attempt);
        yield* drizzle.update(organizationPairing).set({
          removalEndpoints: current.removalEndpoints.map((entry) => entry.machineId === endpoint.machineId
            ? { status: "confirmed", machineId: entry.machineId } : entry),
        }).where(eq(organizationPairing.organizationId, organizationId));
      }));
    }).pipe(Effect.ignore), { concurrency: 4, discard: true });
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const current = yield* loadCurrentAttempt(attempt);
      const confirmed = current.removalEndpoints.every((endpoint) => endpoint.status === "confirmed");
      if (confirmed) yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
      return { confirmed, endpoints: current.removalEndpoints.map((endpoint) => ({
        machineId: endpoint.machineId,
        status: endpoint.status === "confirmed" ? "confirmed" as const : "unconfirmed" as const,
      })) };
    }));
  },
);

/** Only the explicitly admitted destructive teardown can use retained old credentials. */
export const loadTeardownConnections = Effect.fn("PairingRemoval.teardownConnections")(
  function* (organizationId: string) {
    const { drizzle } = yield* Database;
    const [pairing] = yield* drizzle.select().from(organizationPairing)
      .where(eq(organizationPairing.organizationId, organizationId));
    const connections: Connection[] = [];
    const endpoints = yield* decodeEndpoints(pairing?.removalEndpoints ?? []);
    for (const endpoint of endpoints) {
      if (endpoint.status === "pending") connections.push({
        machine_id: endpoint.machineId,
        management: yield* decrypt(endpoint.encryptedExpected),
      });
    }
    return connections;
  },
);
