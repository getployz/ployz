import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { MachineId } from "@ployz/sdk";
import { and, eq, gt, isNull, ne, or, sql } from "drizzle-orm";
import { Data, Effect, Option, Schema } from "effect";
import { storeSystem } from "#/modules/config-store/config-store.server";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import {
  enrollmentAllocation, machineEnrollmentToken, machineRemoveAttempt, organizationMachine, serverAccess,
} from "#/modules/machines/tables";
import { cancelOpenServerEnrollments } from "#/modules/machines/waiting/enrollment-run.server";
import { clusterRead, OrganizationRuntime, SESSION_CLOSE_CLUSTER_MS, withClusterBudget } from "#/modules/runtime/organization-runtime.server";
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

/** The advisory lock class of {@link lockOrganizationEnrollment}; the two-key form keeps it apart from every one-key lock. */
const ORGANIZATION_ENROLLMENT_LOCK = 1_349_601;

/**
 * Serializes the writers that create an Organization's enrollments or tokens, or delete its enrollment state: Enroll,
 * a legacy claim, Fleet Token creation, Forget Servers, a pairing removal, a dead-claim release and Organization
 * removal. The pairing row can't do it, since it doesn't exist before the first server founds. Taken first in its
 * transaction; Organization removal alone then locks the Organization row, the row a claim locks without this lock. The
 * order after that is the pairing row, then `fleet_token`, `waiting_server` and `server_enrollment` rows (each in id
 * order), then enrollment tokens, then `operation_outbox` cancel events (in attempt id order). Cancel, Reject and the
 * run and sweep closers only end attempts; they skip this lock and start at `server_enrollment`, Reject after its
 * `waiting_server` rows. Fleet Token revoke and probe selection skip it too.
 * Revoke locks its token, then that token's `waiting_server` rows, and probe selection locks only `waiting_server` rows. A
 * check-in holds at most one `waiting_server` row, after its token, and stamps a probe handout on that same row. A cancel
 * event's outbox row stays locked until commit, so no path inserts one before it holds that attempt.
 */
export const lockOrganizationEnrollment = Effect.fn("PairingRemoval.lockOrganizationEnrollment")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`select pg_advisory_xact_lock(${ORGANIZATION_ENROLLMENT_LOCK}, hashtext(${organizationId}))`);
});

/**
 * The shared side of {@link lockOrganizationEnrollment}, for arrival enrolls. They add joiners to a founded
 * Organization, so they only need every writer above kept out, not each other; their token's row lock counts them.
 */
export const shareOrganizationEnrollment = Effect.fn("PairingRemoval.shareOrganizationEnrollment")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(sql`select pg_advisory_xact_lock_shared(${ORGANIZATION_ENROLLMENT_LOCK}, hashtext(${organizationId}))`);
});

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

/** Move usable credentials out of ordinary lookup before doing any remote work. */
export const disableOrganizationPairing = Effect.fn("PairingRemoval.disable")(
  function* (organizationId: string, options: { readonly deadClaimOnly?: boolean } = {}) {
    const database = yield* Database;
    const disabled = yield* database.transaction(Effect.gen(function* () {
      yield* lockOrganizationEnrollment(organizationId);
      const { drizzle } = yield* Database;
      const [pairing] = yield* drizzle.select().from(organizationPairing)
        .where(eq(organizationPairing.organizationId, organizationId)).for("update");
      if (!pairing) return null;
      const deadClaim = options.deadClaimOnly === true && pairing.removalStartedAt === null;
      if (deadClaim && (pairing.founderMachineId !== null || (yield* foundingClaimLive(pairing)))) return "kept" as const;
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
        const waived = deadClaim ? pairing.founderClaimMachineId : null;
        for (const machineId of intendedMachines) {
          if (machineId !== waived && !removalEndpoints.some((endpoint) => endpoint.machineId === machineId)) {
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
        attempt = { ...pairing, removalStartedAt, removalEndpoints };
      }
      // A dead claim's own tokens are spent or expired, so other tokens and open attempts stay for the next first server.
      // A full removal clears them even when it resumes a removal a dead-claim release started.
      if (options.deadClaimOnly !== true) yield* deleteEnrollmentTokens(organizationId);
      return { attempt, generation };
    }));
    if (disabled === "kept") return disabled;
    if (disabled !== null) {
      const runtime = yield* OrganizationRuntime;
      yield* runtime.cancel(organizationId, disabled.generation);
    }
    return disabled?.attempt ?? null;
  },
);

/** The witnessed pairing, locked for this transaction; null when it's gone, or no longer `generation`. */
const lockWitnessedPairing = Effect.fn("PairingRemoval.lockWitnessed")(function* (organizationId: string, generation: string) {
  yield* lockOrganizationEnrollment(organizationId);
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select().from(organizationPairing)
    .where(eq(organizationPairing.organizationId, organizationId)).for("update");
  if (!pairing) return { kind: "gone" } as const;
  const current = createHash("sha256").update(yield* decrypt(pairing.encryptedPairingSecret)).digest("hex");
  return current === generation && pairing.removalStartedAt === null ? { kind: "witnessed" } as const : { kind: "replaced" } as const;
});

/** Whether a Server of pairing `generation` other than its removed ones, and other than `except`, is still enrolled. */
const othersRemain = Effect.fn("PairingRemoval.othersRemain")(function* (organizationId: string, generation: string, except?: MachineId) {
  const { drizzle } = yield* Database;
  const [other] = yield* drizzle.select({ machineId: organizationMachine.machineId }).from(organizationMachine)
    .where(and(
      eq(organizationMachine.organizationId, organizationId), eq(organizationMachine.clusterKey, generation),
      except === undefined ? undefined : ne(organizationMachine.machineId, except),
    )).limit(1);
  return other !== undefined;
});

const replaced = { kind: "kept", reason: "Cloud was paired with a new Cluster since." } satisfies ServerRelease;

/**
 * Server `machineId` of the pairing whose generation Cloud witnessed, `generation`, left the Cluster under Cloud's own
 * connection: reset, which took every key Cloud held there, or only taken out (`membership`), keeping its state. While
 * that pairing is still the current one, Cloud drops that Server's row of it (its device keys cascade); a pairing that
 * changed since is left untouched. `last` means a reset took the pairing's last Server: the caller clears the Store's
 * Applied State, then `forgetEmptiedPairing`. A pairing already gone was forgotten by an earlier run. A Server the
 * Cluster had already forgotten (`unconfirmed`) may still hold Cloud's key, so its row goes only while another Server
 * keeps the pairing; as the last, nothing is dropped and the answer is `unconfirmed_last`.
 */
export const dropRemovedServer = Effect.fn("PairingRemoval.dropRemoved")(
  function* (input: { organizationId: string; machineId: MachineId; generation: string; removal: "reset" | "membership" | "unconfirmed" }) {
    const { organizationId, generation } = input;
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      const pairing = yield* lockWitnessedPairing(organizationId, generation);
      if (pairing.kind === "gone") return { kind: "released" } as const;
      if (pairing.kind === "replaced") return replaced;
      if (input.removal === "unconfirmed" && !(yield* othersRemain(organizationId, generation, input.machineId))) {
        return { kind: "unconfirmed_last" } as const;
      }
      const { drizzle } = yield* Database;
      yield* drizzle.delete(organizationMachine).where(and(
        eq(organizationMachine.organizationId, organizationId),
        eq(organizationMachine.clusterKey, generation),
        eq(organizationMachine.machineId, input.machineId),
      ));
      yield* drizzle.update(machineEnrollmentToken).set({ expiresAt: sql`now()`, updatedAt: new Date() }).where(and(
        eq(machineEnrollmentToken.organizationId, organizationId),
        or(eq(machineEnrollmentToken.claimedMachineId, input.machineId), eq(machineEnrollmentToken.joinedMachineId, input.machineId)),
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
      yield* deleteEnrollmentTokens(organizationId);
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
    yield* deleteEnrollmentTokens(organizationId);
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
 * Clear one Machine's device holders (`cli-*`), its push target, then its `cloud` Management Client. Only a successful Clear or an
 * authenticated cleared response confirms removal; once `cloud` is gone, Cloud can't reach the device holders. A Machine that
 * authenticates but never confirms still holds the pairing, and Cloud records that it answered.
 */
const removeEndpointPairing = Effect.fn("PairingRemoval.removeEndpoint")(
  function* (machineId: MachineId, management: string) {
    const ployz = yield* Ployz;
    return yield* Effect.scoped(Effect.gen(function* () {
      const session = yield* ployz.connect({ connections: [{ machine_id: machineId, management }], timeoutMs: 10_000 }, { closeWithin: clusterRead });
      return yield* Effect.gen(function* () {
        const { management_clients: labels } = yield* session.inspect();
        for (const label of labels) {
          if (label.startsWith("cli-")) yield* session.clearManagementClient(label);
        }
        yield* session.setPushTarget(null).pipe(Effect.catchTag("ServerLacks", () => Effect.void));
        yield* session.clearManagementClient("cloud");
        return "confirmed" as const;
      }).pipe(Effect.orElseSucceed(() => "answered" as const));
    })).pipe(withClusterBudget(SESSION_CLOSE_CLUSTER_MS), Effect.catch((error) => Effect.succeed(
      error._tag === "PloyzProviderError" && error.operation === "connect"
        && Option.isSome(Schema.decodeUnknownOption(ManagementClientCleared)(error.cause)) ? "confirmed" as const : "silent" as const,
    )));
  },
);

/** Endpoint failures are unconfirmed outcomes, never evidence that old access was revoked. */
export const revokeOrganizationPairing = Effect.fn("PairingRemoval.revoke")(
  function* (organizationId: string, options: { readonly deadClaimOnly?: boolean } = {}) {
    const attempt = yield* disableOrganizationPairing(organizationId, options);
    if (attempt === "kept") return { confirmed: false, endpoints: [] };
    const database = yield* Database;
    if (attempt === null) {
      const candidates = yield* database.drizzle.select({ machineId: organizationMachine.machineId })
        .from(organizationMachine).where(eq(organizationMachine.organizationId, organizationId));
      return { confirmed: candidates.length === 0, endpoints: candidates.map(({ machineId }) => ({ machineId, status: "unconfirmed" as const })) };
    }
    yield* Effect.forEach(attempt.removalEndpoints, (endpoint) => Effect.gen(function* () {
      if (endpoint.status !== "pending") return;
      const heard = yield* decrypt(endpoint.encryptedExpected).pipe(
        Effect.flatMap((management) => removeEndpointPairing(endpoint.machineId, management)),
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
      if (confirmed) yield* drizzle.delete(organizationPairing).where(eq(organizationPairing.organizationId, organizationId));
      return { confirmed, endpoints: current.removalEndpoints.map((endpoint) => ({
        machineId: endpoint.machineId,
        status: endpoint.status === "confirmed" ? "confirmed" as const : "unconfirmed" as const,
      })) };
    }));
  },
);
