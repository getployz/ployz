import "@tanstack/react-start/server-only";
import type { MachineId, Signed } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect, Option, Schema } from "effect";
import { Database } from "#/server/database.server";
import { volumeMirrorStatus } from "#/modules/volume-run/tables";

/**
 * The Mirror approvals Cloud keeps for one Mirror, newest first.
 *
 * A Mirror admits an approval when a pull carrying it passes admission, before the pull receives anything, and
 * admitting one ends the approval that was open there, for good. So one approval works at the Mirror, and only the
 * Mirror knows which: an answer can be lost or come late, a pull can fail after admission, and a run and a sync can
 * overlap. Cloud never infers it from which step succeeded or in what order answers came. It keeps every approval it
 * sent toward the Mirror and drops one only when no order of events lets it be the open one:
 *
 * - `offerApproval` adds an approval before any step carries it.
 * - `settleApprovals` runs when the Mirror accepted a pull carrying X that Cloud sent at `sentAt`. X was the open
 *   approval when the Mirror accepted it, so any other was not, and one whose expiry passed well before `sentAt` can
 *   never be admitted again. Those go.
 * - Deleting the Mirror drops them all.
 *
 * A sync tries the kept approvals newest first and moves on only past a refusal for the approval.
 */
export const sameApproval = (a: Signed, b: Signed) => a.body === b.body;

/** Past its expiry, how long before an approval may go: well beyond any clock skew between Cloud and a Mirror. */
export const SETTLED_AFTER_MS = 60 * 60 * 1000;

const ApprovalWindow = Schema.fromJsonString(Schema.Struct({ expires_at: Schema.Number }));

/** Whether `approval` expired too long before `sentAt` for any Mirror to admit it then or later. An unreadable one stays. */
export const settledBefore = (approval: Signed, sentAt: number) => {
  const window = Schema.decodeUnknownOption(ApprovalWindow)(approval.body);
  return Option.isSome(window) && window.value.expires_at + SETTLED_AFTER_MS < sentAt;
};

const keptFor = (volumeId: string, mirror: MachineId) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ approvals: volumeMirrorStatus.approvals }).from(volumeMirrorStatus)
    .where(and(eq(volumeMirrorStatus.volumeId, volumeId), eq(volumeMirrorStatus.machineId, mirror)))
    .for("update");
  return row;
});

/** Keeps `approval` with the Mirror on `mirror`, newest, before any step carries it. */
export const offerApproval = Effect.fn("MirrorApprovals.offer")(function* (organizationId: string, volumeId: string, mirror: MachineId, approval: Signed) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const row = yield* keptFor(volumeId, mirror);
    const approvals = [approval, ...(row?.approvals ?? []).filter((kept) => !sameApproval(kept, approval))];
    const now = new Date();
    yield* database.drizzle.insert(volumeMirrorStatus)
      .values({ organizationId, volumeId, machineId: mirror, approvals, updatedAt: now })
      .onConflictDoUpdate({ target: [volumeMirrorStatus.volumeId, volumeMirrorStatus.machineId], set: { approvals, updatedAt: now } });
  }));
});

/** The Mirror on `mirror` accepted a pull carrying `accepted` that Cloud sent at `sentAt`: drops what it settled. */
export const settleApprovals = Effect.fn("MirrorApprovals.settle")(function* (volumeId: string, mirror: MachineId, accepted: Signed, sentAt: number) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const kept = (yield* keptFor(volumeId, mirror))?.approvals ?? null;
    if (kept === null) return;
    const approvals = kept.filter((entry) => sameApproval(entry, accepted) || !settledBefore(entry, sentAt));
    if (approvals.length === kept.length) return;
    yield* database.drizzle.update(volumeMirrorStatus).set({ approvals, updatedAt: new Date() })
      .where(and(eq(volumeMirrorStatus.volumeId, volumeId), eq(volumeMirrorStatus.machineId, mirror)));
  }));
});

/** Drops every approval kept with the Mirrors on `machines`, once their copies are gone. */
export const dropApprovals = Effect.fn("MirrorApprovals.drop")(function* (volumeId: string, machines: readonly MachineId[]) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumeMirrorStatus).set({ approvals: null, updatedAt: new Date() })
    .where(and(eq(volumeMirrorStatus.volumeId, volumeId), inArray(volumeMirrorStatus.machineId, [...machines])));
});
