import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { MachineId } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Data, Effect, Option, Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { enrollmentAllocation, machineEnrollmentToken, organizationMachine, serverAccess } from "#/modules/machines/tables";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { Ployz } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { RemovalEndpoints, type RemovalEndpoint } from "#/modules/machines/pairing-removal";
import type { ServerRelease } from "#/modules/machines/machine-removal";

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
        // Every device's capability goes at once; clearing each Server's `cli-*` holders below confirms it.
        yield* drizzle.delete(serverAccess).where(eq(serverAccess.organizationId, organizationId));
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

/**
 * Server `machineId` of the pairing whose generation Cloud witnessed, `generation`, left the Cluster under Cloud's own
 * connection: reset, which took every key Cloud held there, or only taken out (`membership`), keeping its state. Cloud drops that Server's row (its
 * device keys cascade). When no other Server of that pairing remains, the Cluster went with it: Cloud forgets the
 * pairing and its enrollment tokens outright, with no Clear to confirm. A pairing that changed since, or another
 * Server, keeps Cloud's hold, so a replay after a new Server was paired forgets nothing.
 */
export const releaseRemovedServer = Effect.fn("PairingRemoval.releaseRemoved")(
  function* (input: { organizationId: string; machineId: string; generation: string; removal: "reset" | "membership" }) {
    const { organizationId } = input;
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [pairing] = yield* drizzle.select().from(organizationPairing)
        .where(eq(organizationPairing.organizationId, organizationId)).for("update");
      // SAFETY: a Machine ID the SDK removed; an organization_machine row only matches the same representation.
      yield* drizzle.delete(organizationMachine).where(and(
        eq(organizationMachine.organizationId, organizationId),
        eq(organizationMachine.machineId, input.machineId as MachineId),
      ));
      // Gone already: this is a replay of a release that forgot it.
      if (!pairing) return { kind: "released" } satisfies ServerRelease;
      const generation = createHash("sha256").update(yield* decrypt(pairing.encryptedPairingSecret)).digest("hex");
      if (generation !== input.generation || pairing.removalStartedAt !== null) {
        return { kind: "kept", reason: "Cloud was paired with a new Cluster since." } satisfies ServerRelease;
      }
      const [other] = yield* drizzle.select({ machineId: organizationMachine.machineId }).from(organizationMachine)
        .where(eq(organizationMachine.organizationId, organizationId)).limit(1);
      if (other) return { kind: "others_remain" } satisfies ServerRelease;
      if (input.removal === "membership") {
        return { kind: "kept", reason: "it left the Cluster without a reset, so Cloud keeps the pairing." } satisfies ServerRelease;
      }
      yield* drizzle.delete(machineEnrollmentToken).where(eq(machineEnrollmentToken.organizationId, organizationId));
      yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
      return { kind: "released" } satisfies ServerRelease;
    }));
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

const ManagementClientCleared = Schema.Struct({
  code: Schema.Literal("unauthenticated"),
  details: Schema.Struct({ management_client: Schema.Literal("cleared") }),
});

/**
 * Clear one Machine's device holders (`cli-*`), then its `cloud` Management Client. Only a successful Clear or an
 * authenticated cleared response confirms removal; once `cloud` is gone, Cloud can't reach the device holders.
 */
const removeEndpointPairing = Effect.fn("PairingRemoval.removeEndpoint")(
  function* (machineId: MachineId, management: string) {
    const ployz = yield* Ployz;
    return yield* Effect.scoped(Effect.gen(function* () {
      const session = yield* ployz.connect({ connections: [{ machine_id: machineId, management }], timeoutMs: 10_000 });
      const { management_clients: labels } = yield* session.inspect();
      for (const label of labels) {
        if (label.startsWith("cli-")) yield* session.clearManagementClient(label);
      }
      yield* session.clearManagementClient("cloud");
      return true;
    })).pipe(Effect.catch((error) => Effect.succeed(
      error._tag === "PloyzProviderError" && error.operation === "connect"
        && Option.isSome(Schema.decodeUnknownOption(ManagementClientCleared)(error.cause)),
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
