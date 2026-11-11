import "@tanstack/react-start/server-only";
import type { MachineId, Signed } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { volumeMirrorStatus } from "#/modules/volume-run/tables";

/**
 * The Mirror approvals Cloud keeps for one Mirror, newest first.
 *
 * A Mirror admits an approval when a pull carrying it passes admission, before the pull receives anything, and
 * admitting one ends the approval that was open there, for good. So one approval works at the Mirror, and only the
 * Mirror knows which: an answer can be lost or come late, a pull can fail after admission, a run and a sync can
 * overlap, and a Mirror that doesn't enforce accepts any pull without admitting anything. Cloud never infers which from
 * an accepted pull or from the order answers came in. It drops an approval only on the Mirror's word that the approval
 * isn't open there and never will be:
 *
 * - `offerApproval` adds an approval before any step carries it.
 * - `dropSpent` drops the approvals a Mirror refused as spent: one that already ran and ended there, or one that
 *   expired before the Mirror ever admitted it (see `CopyAnswer.spent`). Cloud never sends them again.
 * - Deleting the Mirror drops them all.
 *
 * A sync tries the kept approvals newest first and moves on only past a refusal for the approval. An ended approval
 * behind the open one is never tried, so it stays: the list grows by one for each time a Device approves the Mirror
 * again.
 */
export const sameApproval = (a: Signed, b: Signed) => a.body === b.body;

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

/** Drops the approvals the Mirror on `mirror` refused as `spent`. */
export const dropSpent = Effect.fn("MirrorApprovals.dropSpent")(function* (volumeId: string, mirror: MachineId, spent: readonly Signed[]) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const kept = (yield* keptFor(volumeId, mirror))?.approvals ?? null;
    if (kept === null) return;
    const approvals = kept.filter((entry) => !spent.some((gone) => sameApproval(gone, entry)));
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
