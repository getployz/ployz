import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import type { MachineId } from "@ployz/sdk";
import { and, eq, ne, or, sql } from "drizzle-orm";
import { Effect } from "effect";
import { requestBillingSync } from "#/modules/billing/billing-sync.server";
import { machineEnrollmentToken, organizationMachine } from "#/modules/machines/tables";
import type { ServerRelease } from "#/modules/machines/machine-removal";
import { organizationPairing } from "#/modules/runtime/tables";
import { Database } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";

// What lets go of a removed Server's row, apart from the pairing removal that dials the Cluster, so the Organization
// Runtime's frame records can call it without importing that Runtime back.

export const decrypt = Effect.fn("PairingRemoval.decrypt")(function* (
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
 * event's outbox row stays locked until commit, so no path inserts one before it holds that attempt. A reset that ends a
 * founding claim expires the claim's tokens after its founder attempt's cancel event; that row is new, so nothing waits on it.
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

/** The witnessed pairing, locked for this transaction; null when it's gone, or no longer `generation`. */
export const lockWitnessedPairing = Effect.fn("PairingRemoval.lockWitnessed")(function* (organizationId: string, generation: string) {
  yield* lockOrganizationEnrollment(organizationId);
  const { drizzle } = yield* Database;
  const [pairing] = yield* drizzle.select().from(organizationPairing)
    .where(eq(organizationPairing.organizationId, organizationId)).for("update");
  if (!pairing) return { kind: "gone" } as const;
  const current = createHash("sha256").update(yield* decrypt(pairing.encryptedPairingSecret)).digest("hex");
  return current === generation && pairing.removalStartedAt === null ? { kind: "witnessed" } as const : { kind: "replaced" } as const;
});

/** Whether a Server of pairing `generation` other than its removed ones, and other than `except`, is still enrolled. */
export const othersRemain = Effect.fn("PairingRemoval.othersRemain")(function* (organizationId: string, generation: string, except?: MachineId) {
  const { drizzle } = yield* Database;
  const [other] = yield* drizzle.select({ machineId: organizationMachine.machineId }).from(organizationMachine)
    .where(and(
      eq(organizationMachine.organizationId, organizationId), eq(organizationMachine.clusterKey, generation),
      except === undefined ? undefined : ne(organizationMachine.machineId, except),
    )).limit(1);
  return other !== undefined;
});

export const replaced = { kind: "kept", reason: "Cloud was paired with a new Cluster since." } satisfies ServerRelease;

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
    const released = yield* database.transaction(Effect.gen(function* () {
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
    yield* requestBillingSync(organizationId, "server-removed");
    return released;
  },
);

