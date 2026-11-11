import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { MachineId } from "@ployz/sdk";
import { and, eq, gt, isNull, sql } from "drizzle-orm";
import { Data, Effect, Schema } from "effect";
import { endCloudflareSetup } from "#/modules/cloudflare/cluster-gone.server";
import { storeSystem } from "#/modules/config-store/config-store.server";
import { releasingExpectation } from "#/modules/machines/connections.server";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import {
  clusterSealingKey, clusterSealingKeyGenerator, enrollmentAllocation, machineEnrollmentToken, machineRemoveAttempt,
  organizationMachine, serverAccess,
} from "#/modules/machines/tables";
import { cancelOpenServerEnrollments, cancelServerEnrollments, openFounderAttempt } from "#/modules/machines/waiting/enrollment-run.server";
import { clusterRead, OrganizationRuntime, SESSION_CLOSE_CLUSTER_MS, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import { managementClientCleared, Ployz } from "#/modules/runtime/ployz.server";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { RemovalEndpoints, type RemovalEndpoint } from "#/modules/machines/pairing-removal";
import {
  decrypt, dropRemovedServer, lockOrganizationEnrollment, lockWitnessedPairing, othersRemain, replaced,
  shareOrganizationEnrollment,
} from "#/modules/machines/pairing-release.server";

export { dropRemovedServer, lockOrganizationEnrollment, shareOrganizationEnrollment };
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

 export const deleteEnrollmentTokens = Effect.fn("PairingRemoval.deleteEnrollmentTokens")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  yield* cancelOpenServerEnrollments(organizationId);
  yield* drizzle.delete(machineEnrollmentToken).where(eq(machineEnrollmentToken.organizationId, organizationId));
});

/** A claim lives while any unspent, unexpired founding token its Machine holds with its key could still complete it. */
const foundingClaimLive = Effect.fn("PairingRemoval.foundingClaimLive")(function* (pairing: Pairing) {
  const { drizzle } = yield* Database;
  const [live] = yield* drizzle.select({ id: machineEnrollmentToken.id }).from(machineEnrollmentToken).where(and(
    eq(machineEnrollmentToken.organizationId, pairing.organizationId),
    eq(machineEnrollmentToken.claimedMachineId, pairing.founderClaimMachineId),
    pairing.founderPublicKey === null ? undefined : eq(machineEnrollmentToken.claimedPublicKey, pairing.founderPublicKey),
    sql`${machineEnrollmentToken.enrollmentRole} is distinct from 'joiner'`,
    isNull(machineEnrollmentToken.joinedMachineId),
    gt(machineEnrollmentToken.expiresAt, sql`now()`),
  )).limit(1);
  return live !== undefined;
});

/** A founding claim no token can finish, which the next Enroll drops before it plans a first server, unless that server made it. */
export const isDeadFoundingClaim = Effect.fn("PairingRemoval.isDeadFoundingClaim")(function* (pairing: Pairing) {
  return pairing.founderMachineId === null && pairing.removalStartedAt === null && !(yield* foundingClaimLive(pairing));
});

/**
 * How a removal treats an unfounded pairing's founding claim. `deadClaimOnly` (Enroll) removes the pairing only once no
 * token can finish the claim, keeping the Organization's tokens. `releaseClaim` (Reset, whose user confirmed the claiming
 * Machine is stopped or erased) ends the claim itself first, by expiring its tokens and cancelling the founder's attempt,
 * then removes the pairing the same way. It refuses, writing nothing, when another Machine may hold the pairing unconfirmably.
 */
type DisableOptions = { readonly deadClaimOnly?: boolean; readonly releaseClaim?: boolean };

/** Move usable credentials out of ordinary lookup before doing any remote work. */
export const disableOrganizationPairing = Effect.fn("PairingRemoval.disable")(
  function* (organizationId: string, options: DisableOptions = {}) {
    const database = yield* Database;
    const disabled = yield* database.transaction(Effect.gen(function* () {
      yield* lockOrganizationEnrollment(organizationId);
      const { drizzle } = yield* Database;
      const [pairing] = yield* drizzle.select().from(organizationPairing)
        .where(eq(organizationPairing.organizationId, organizationId)).for("update");
      if (!pairing) return null;
      const release = options.releaseClaim === true;
      const waivesClaim = options.deadClaimOnly === true || release;
      const deadClaim = waivesClaim && pairing.removalStartedAt === null;
      const kept = release
        ? pairing.founderMachineId !== null
        : deadClaim && (pairing.founderMachineId !== null || (yield* foundingClaimLive(pairing)));
      if (kept) return "kept" as const;
      const secret = yield* decrypt(pairing.encryptedPairingSecret);
      const generation = createHash("sha256").update(secret).digest("hex");
      let attempt: RemovalAttempt;
      if (pairing.removalStartedAt !== null && pairing.removalEndpoints !== null) {
        const resumed = yield* decodeEndpoints(pairing.removalEndpoints);
        // An unpublished claim left no capability to clear, so once its tokens are expired it is waived as a fresh release waives it.
        const removalEndpoints = release
          ? resumed.filter((endpoint) => !(endpoint.machineId === pairing.founderClaimMachineId && endpoint.status === "unknown"))
          : resumed;
        if (release) {
          yield* endFoundingClaim(pairing);
          if (removalEndpoints.length !== resumed.length) {
            yield* drizzle.update(organizationPairing).set({ removalEndpoints })
              .where(eq(organizationPairing.organizationId, organizationId));
          }
        }
        attempt = { ...pairing, removalStartedAt: pairing.removalStartedAt, removalEndpoints };
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
        const waived = deadClaim ? pairing.founderClaimMachineId : null;
        for (const machineId of intendedMachines) {
          if (machineId !== waived && !removalEndpoints.some((endpoint) => endpoint.machineId === machineId)) {
            removalEndpoints.push({ machineId: yield* Schema.decodeUnknownEffect(rustMachineIdSchema)(machineId), status: "unknown" });
          }
        }
        if (release && removalEndpoints.some((endpoint) => endpoint.status === "unknown")) return "unconfirmable" as const;
        if (release) yield* endFoundingClaim(pairing);
        const removalStartedAt = new Date();
        yield* drizzle.update(organizationPairing).set({ removalStartedAt, removalEndpoints })
          .where(eq(organizationPairing.organizationId, organizationId));
        // This also retires the old generation's preferred-entry flag.
        yield* drizzle.delete(organizationMachine).where(eq(organizationMachine.organizationId, organizationId));
        // Every device's capability goes at once; clearing each Server's `cli-*` holders below confirms it.
        yield* drizzle.delete(serverAccess).where(eq(serverAccess.organizationId, organizationId));
        attempt = { ...pairing, removalStartedAt, removalEndpoints };
      }
      // A dead claim's own tokens are spent or expired, so other tokens and open attempts stay for the next first server.
      // A full removal clears them even when it resumes a removal a dead-claim release started.
      if (!waivesClaim) yield* deleteEnrollmentTokens(organizationId);
      return { attempt, generation };
    }));
    if (disabled === "kept" || disabled === "unconfirmable") return disabled;
    if (disabled !== null) {
      const runtime = yield* OrganizationRuntime;
      yield* runtime.cancel(organizationId, disabled.generation);
    }
    return disabled?.attempt ?? null;
  },
);

/**
 * Ends an unfounded pairing's founding claim: cancels the founder's open attempt, then expires every founding token its
 * Machine claimed, so it can neither publish nor complete. Held under the enrollment lock and the pairing row.
 */
const endFoundingClaim = Effect.fn("PairingRemoval.endFoundingClaim")(function* (pairing: Pairing) {
  const founding = yield* openFounderAttempt(pairing.organizationId);
  if (founding !== undefined) yield* cancelServerEnrollments(pairing.organizationId, [founding.id]);
  const { drizzle } = yield* Database;
  yield* drizzle.update(machineEnrollmentToken).set({ expiresAt: sql`now()`, updatedAt: new Date() }).where(and(
    eq(machineEnrollmentToken.organizationId, pairing.organizationId),
    eq(machineEnrollmentToken.claimedMachineId, pairing.founderClaimMachineId),
    sql`${machineEnrollmentToken.enrollmentRole} is distinct from 'joiner'`,
    isNull(machineEnrollmentToken.joinedMachineId),
    gt(machineEnrollmentToken.expiresAt, sql`now()`),
  ));
});

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
      yield* deleteEnrollmentTokens(organizationId);
      yield* drizzle.delete(clusterSealingKey).where(eq(clusterSealingKey.organizationId, organizationId));
      yield* drizzle.delete(clusterSealingKeyGenerator).where(eq(clusterSealingKeyGenerator.organizationId, organizationId));
      yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
      yield* endCloudflareSetup(organizationId);
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
  yield* lockOrganizationEnrollment(organizationId);
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
 * (device keys cascade), its Sealing public key and generator, allocations, enrollment tokens and the Server removals it
 * was attempting. The Organization's Sealing Key and its recovery backup stay, so a later Server can be given the key. The
 * Store commits `cluster_forgotten` on its own and it is idempotent, so if the Cloud-row transaction then fails, running
 * Forget Servers again finishes it.
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
    yield* drizzle.delete(clusterSealingKey).where(eq(clusterSealingKey.organizationId, organizationId));
    yield* drizzle.delete(clusterSealingKeyGenerator).where(eq(clusterSealingKeyGenerator.organizationId, organizationId));
    yield* deleteEnrollmentTokens(organizationId);
    yield* drizzle.delete(enrollmentAllocation).where(eq(enrollmentAllocation.organizationId, organizationId));
    yield* drizzle.delete(machineRemoveAttempt).where(eq(machineRemoveAttempt.organizationId, organizationId));
    yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
    yield* endCloudflareSetup(organizationId);
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

/**
 * Clear one Machine's device holders (`cli-*`), its push target, then its `cloud` Management Client. Only a successful Clear or an
 * authenticated cleared response confirms removal; once `cloud` is gone, Cloud can't reach the device holders. A Machine that
 * authenticates but never confirms still holds the pairing, and Cloud records that it answered. One that now answers for
 * another Cluster than the pairing's, `expect`, stays silent, as one that doesn't answer does: the removal clears every
 * device holder it lists, and another Cluster's are not this pairing's to clear. A revocation's Clear names only the
 * `cli-*` holder Cloud gave one credential, so it expects nothing.
 */
const removeEndpointPairing = Effect.fn("PairingRemoval.removeEndpoint")(
  function* (machineId: MachineId, management: string, expect: { readonly sealingPublicKey: string; readonly allowUnverified: boolean } | null) {
    const ployz = yield* Ployz;
    return yield* Effect.scoped(Effect.gen(function* () {
      const session = yield* ployz.connect({ connections: [{ machine_id: machineId, management }], timeoutMs: 10_000, expect }, { closeWithin: clusterRead });
      return yield* Effect.gen(function* () {
        const { management_clients: labels } = yield* session.inspect();
        for (const label of labels) {
          if (label.startsWith("cli-")) yield* session.clearManagementClient(label);
        }
        yield* session.setPushTarget(null).pipe(Effect.catchTag("ServerLacks", () => Effect.void));
        yield* session.clearManagementClient("cloud");
        return "confirmed" as const;
      }).pipe(Effect.orElseSucceed(() => "answered" as const));
    })).pipe(withClusterBudget(SESSION_CLOSE_CLUSTER_MS), Effect.catch((error) =>
      managementClientCleared(error)
        ? Effect.succeed("confirmed" as const)
        : Effect.logInfo("Server didn't confirm its removal yet.", { machineId, error }).pipe(Effect.as("silent" as const)),
    ));
  },
);

/** Endpoint failures are unconfirmed outcomes, never evidence that old access was revoked. */
export const revokeOrganizationPairing = Effect.fn("PairingRemoval.revoke")(
  function* (organizationId: string, options: DisableOptions = {}) {
    const attempt = yield* disableOrganizationPairing(organizationId, options);
    if (attempt === "kept" || attempt === "unconfirmable") return { confirmed: false, endpoints: [], refused: attempt };
    const database = yield* Database;
    if (attempt === null) {
      const candidates = yield* database.drizzle.select({ machineId: organizationMachine.machineId })
        .from(organizationMachine).where(eq(organizationMachine.organizationId, organizationId));
      return { confirmed: candidates.length === 0, endpoints: candidates.map(({ machineId }) => ({ machineId, status: "unconfirmed" as const })) };
    }
    const generation = createHash("sha256").update(yield* decrypt(attempt.encryptedPairingSecret)).digest("hex");
    const expect = yield* releasingExpectation(organizationId, generation);
    yield* Effect.forEach(attempt.removalEndpoints, (endpoint) => Effect.gen(function* () {
      if (endpoint.status !== "pending") return;
      const heard = yield* decrypt(endpoint.encryptedExpected).pipe(
        Effect.flatMap((management) => removeEndpointPairing(endpoint.machineId, management, expect)),
        Effect.orElseSucceed(() => "silent" as const),
      );
      if (heard === "silent") return;
      yield* database.transaction(Effect.gen(function* () {
        const { drizzle } = yield* Database;
        const current = yield* loadCurrentAttempt(attempt);
        yield* drizzle.update(organizationPairing).set(heard === "answered" ? { removalAnsweredAt: sql`clock_timestamp()` } : {
          removalEndpoints: current.removalEndpoints.map((entry) => entry.machineId === endpoint.machineId
            ? { status: "confirmed", machineId: entry.machineId } : entry),
        }).where(eq(organizationPairing.organizationId, organizationId));
      }));
    }), { concurrency: 4, discard: true });
    return yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const current = yield* loadCurrentAttempt(attempt);
      const confirmed = current.removalEndpoints.every((endpoint) => endpoint.status === "confirmed");
      if (confirmed) {
        yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
        yield* endCloudflareSetup(organizationId);
      }
      return { confirmed, endpoints: current.removalEndpoints.map((endpoint) => ({
        machineId: endpoint.machineId,
        status: endpoint.status === "confirmed" ? "confirmed" as const : "unconfirmed" as const,
      })) };
    }));
  },
);
