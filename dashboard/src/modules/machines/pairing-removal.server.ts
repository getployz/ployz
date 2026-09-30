import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { MachineId } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Data, Effect, Option, Schema } from "effect";
import { storeSystem } from "#/modules/config-store/config-store.server";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import {
  enrollmentAllocation, machineEnrollmentToken, machineRemoveAttempt, organizationMachine, serverAccess,
} from "#/modules/machines/tables";
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

/** The witnessed pairing, locked for this transaction; null when it's gone, or no longer `generation`. */
const lockWitnessedPairing = Effect.fn("PairingRemoval.lockWitnessed")(function* (organizationId: string, generation: string) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select().from(organizationPairing)
    .where(eq(organizationPairing.organizationId, organizationId)).for("update");
  if (!pairing) return { kind: "gone" } as const;
  const current = createHash("sha256").update(yield* decrypt(pairing.encryptedPairingSecret)).digest("hex");
  return current === generation && pairing.removalStartedAt === null ? { kind: "witnessed" } as const : { kind: "replaced" } as const;
});

/** Whether a Server of pairing `generation` other than its removed ones is still enrolled. */
const othersRemain = Effect.fn("PairingRemoval.othersRemain")(function* (organizationId: string, generation: string) {
  const { drizzle } = yield* Database;
  const [other] = yield* drizzle.select({ machineId: organizationMachine.machineId }).from(organizationMachine)
    .where(and(eq(organizationMachine.organizationId, organizationId), eq(organizationMachine.clusterKey, generation))).limit(1);
  return other !== undefined;
});

const replaced = { kind: "kept", reason: "Cloud was paired with a new Cluster since." } satisfies ServerRelease;

/**
 * Server `machineId` of the pairing whose generation Cloud witnessed, `generation`, left the Cluster under Cloud's own
 * connection: reset, which took every key Cloud held there, or only taken out (`membership`), keeping its state. While
 * that pairing is still the current one, Cloud drops that Server's row of it (its device keys cascade); a pairing that
 * changed since is left untouched. `last` means a reset took the pairing's last Server: the caller clears the Store's
 * Applied State, then `forgetEmptiedPairing`. A pairing already gone was forgotten by an earlier run.
 */
export const dropRemovedServer = Effect.fn("PairingRemoval.dropRemoved")(
  function* (input: { organizationId: string; machineId: string; generation: string; removal: "reset" | "membership" }) {
    const { organizationId, generation } = input;
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const pairing = yield* lockWitnessedPairing(organizationId, generation);
      if (pairing.kind === "gone") return { kind: "released" } as const;
      if (pairing.kind === "replaced") return replaced;
      const { drizzle } = yield* Database;
      // SAFETY: a Machine ID the SDK removed; an organization_machine row only matches the same representation.
      yield* drizzle.delete(organizationMachine).where(and(
        eq(organizationMachine.organizationId, organizationId),
        eq(organizationMachine.clusterKey, generation),
        eq(organizationMachine.machineId, input.machineId as MachineId),
      ));
      if (yield* othersRemain(organizationId, generation)) return { kind: "others_remain" } as const;
      if (input.removal === "membership") {
        return { kind: "kept", reason: "it left the Cluster without a reset, so Cloud keeps the pairing." } as const;
      }
      return { kind: "last" } as const;
    }));
  },
);

/**
 * The Cluster of pairing `generation` is gone and the Store let go of what ran on it: Cloud forgets the pairing and its
 * enrollment tokens outright, with no Clear to confirm, if it is still that pairing with no Server left.
 */
export const forgetEmptiedPairing = Effect.fn("PairingRemoval.forgetEmptied")(
  function* (organizationId: string, generation: string) {
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const pairing = yield* lockWitnessedPairing(organizationId, generation);
      if (pairing.kind === "gone") return { kind: "released" } satisfies ServerRelease;
      if (pairing.kind === "replaced") return replaced;
      if (yield* othersRemain(organizationId, generation)) return { kind: "others_remain" } satisfies ServerRelease;
      const { drizzle } = yield* Database;
      yield* drizzle.delete(machineEnrollmentToken).where(eq(machineEnrollmentToken.organizationId, organizationId));
      yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
      return { kind: "released" } satisfies ServerRelease;
    }));
  },
);

/** What Forget Servers saw when it tried the Servers: the pairing's generation (none unpaired) and its Servers' rows. */
type ClusterSeen = { readonly generation: string | null; readonly servers: readonly string[] };

/** The Organization's pairing and what Forget Servers compares of it: its generation (none unpaired) and sorted Server rows. */
const cluster = Effect.fn("PairingRemoval.cluster")(function* (organizationId: string, pairing: Pairing | undefined) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ machineId: organizationMachine.machineId }).from(organizationMachine)
    .where(eq(organizationMachine.organizationId, organizationId));
  const generation = pairing ? createHash("sha256").update(yield* decrypt(pairing.encryptedPairingSecret)).digest("hex") : null;
  return { pairing, seen: { generation, servers: rows.map((row) => row.machineId).sort() } satisfies ClusterSeen };
});

/** The Organization's Cluster as Forget Servers sees it before trying the Servers. */
export const seeCluster = Effect.fn("PairingRemoval.seeCluster")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select().from(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
  return yield* cluster(organizationId, pairing);
});

/** As `seeCluster`, holding the pairing row's lock for the caller's transaction. */
const lockCluster = Effect.fn("PairingRemoval.lockCluster")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select().from(organizationPairing)
    .where(eq(organizationPairing.organizationId, organizationId)).for("update");
  return yield* cluster(organizationId, pairing);
});

/**
 * Forget Servers: a user said the Organization's Servers were deleted and Cloud reached none of those it saw (`seen`), so
 * every key Cloud held there went with them and there is nothing to Clear. Under the pairing's lock, a pairing or Server
 * that changed since refuses; otherwise the Store lets go of what ran (`cluster_forgotten`) first, so no new Cluster is
 * founded before it has; then every row of Cloud's hold on the Cluster goes: the pairing (a stuck removal too), its Servers
 * (device keys cascade), allocations, enrollment tokens and the Server removals it was attempting. The Store commits `cluster_forgotten` on its own and it is idempotent, so if the Cloud-row
 * transaction then fails, running Forget Servers again finishes it.
 */
export const forgetCluster = Effect.fn("PairingRemoval.forgetCluster")(function* (organizationId: string, seen: ClusterSeen) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    const now = yield* lockCluster(organizationId);
    if (now.seen.generation !== seen.generation || now.seen.servers.join() !== seen.servers.join()) {
      return { kind: "changed" as const };
    }
    const written = yield* storeSystem(organizationId, { event: "cluster_forgotten" });
    const { drizzle } = yield* Database;
    yield* drizzle.delete(organizationMachine).where(eq(organizationMachine.organizationId, organizationId));
    yield* drizzle.delete(machineEnrollmentToken).where(eq(machineEnrollmentToken.organizationId, organizationId));
    yield* drizzle.delete(enrollmentAllocation).where(eq(enrollmentAllocation.organizationId, organizationId));
    yield* drizzle.delete(machineRemoveAttempt).where(eq(machineRemoveAttempt.organizationId, organizationId));
    yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
    return { kind: "forgotten" as const, cancelled: written.written === "automated" ? written.cancelled : [] };
  }));
});

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
