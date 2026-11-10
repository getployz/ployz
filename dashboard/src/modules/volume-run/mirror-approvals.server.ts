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
 * admitting one ends every older approval for that Mirror there. So at most one approval works at the Mirror, and only
 * the Mirror knows which: an answer can be lost after admission, and a pull can fail after admission. Cloud therefore
 * keeps every approval it has sent toward the Mirror until the Mirror accepts one of them:
 *
 * - `offerApproval` adds an approval before any step carries it.
 * - `acceptApproval` runs when a pull carrying an approval was accepted. It drops every older approval, which the
 *   accepted one ended, and every newer one the Mirror refused, which it never admitted.
 * - A sync tries the kept approvals newest first and moves on only past a refusal for the approval.
 *
 * Nothing else drops an approval, so the one the Mirror admitted is always kept, whatever step failed or went
 * unanswered.
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

/**
 * The Mirror on `mirror` accepted a pull carrying `approval` after refusing `refused`. An approval no longer kept, as
 * after the Mirror's deletion, stays dropped.
 */
export const acceptApproval = Effect.fn("MirrorApprovals.accept")(function* (volumeId: string, mirror: MachineId, approval: Signed, refused: readonly Signed[] = []) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const kept = (yield* keptFor(volumeId, mirror))?.approvals ?? [];
    const at = kept.findIndex((entry) => sameApproval(entry, approval));
    if (at < 0) return;
    const newer = kept.slice(0, at).filter((entry) => !refused.some((no) => sameApproval(no, entry)));
    const approvals = [...newer, approval];
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
