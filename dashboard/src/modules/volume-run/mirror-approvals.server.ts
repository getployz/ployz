import "@tanstack/react-start/server-only";
import type { MachineId, Signed } from "@ployz/sdk";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { Database } from "#/server/database.server";
import { volumeMirrorStatus } from "#/modules/volume-run/tables";

/**
 * The Mirror approvals Cloud keeps for one Mirror, the one the Mirror last accepted or Cloud last offered first.
 *
 * A Mirror admits an approval when a pull carrying it passes admission, before the pull receives anything, and
 * admitting one ends the approval that was open there. So one approval works at the Mirror, and only the Mirror knows
 * which: an answer can be lost or come late, a pull can fail after admission, a run and a sync can overlap, a Mirror
 * that doesn't enforce accepts any pull without admitting anything, and a refusal can stop being true (a clock that
 * steps back, approval records that are reset). So while the Mirror exists Cloud drops none of them, and the one the
 * Mirror holds open is always kept:
 *
 * - `offerApproval` puts an approval first before any step carries it.
 * - `acceptApproval` puts first the approval a pull the Mirror accepted carried, so the next sync tries it first.
 * - Deleting the Mirror drops them all.
 *
 * A sync tries them in order and moves on only past a refusal for the approval. The list grows by one each time a
 * Device approves the Mirror again, up to `MAX_KEPT_APPROVALS` offers. Past it Cloud refuses the new approval before
 * any step carries it, rather than drop a kept one: that could be the open one. A sync's late accept can still put back
 * one it read before the Mirror was deleted and made again, which an earlier offer kept, since the Mirror may hold it
 * open; it never adds a body no offer kept.
 */
export const sameApproval = (a: Signed, b: Signed) => a.body === b.body;

const keptFor = (volumeId: string, mirror: MachineId) => Effect.gen(function* () {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ approvals: volumeMirrorStatus.approvals }).from(volumeMirrorStatus)
    .where(and(eq(volumeMirrorStatus.volumeId, volumeId), eq(volumeMirrorStatus.machineId, mirror)))
    .for("update");
  return row;
});

/** The most approvals Cloud keeps for one Mirror. */
export const MAX_KEPT_APPROVALS = 128;

/**
 * Keeps `approval` with the Mirror on `mirror`, first, before any step carries it. False, keeping nothing new, when the
 * Mirror already keeps `MAX_KEPT_APPROVALS` others.
 */
export const offerApproval = Effect.fn("MirrorApprovals.offer")(function* (organizationId: string, volumeId: string, mirror: MachineId, approval: Signed) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    // The row exists before the lock, so the lock holds even for the first offer: one that read no row would otherwise
    // write its list over a list committed in between.
    yield* database.drizzle.insert(volumeMirrorStatus).values({ organizationId, volumeId, machineId: mirror, updatedAt: new Date() }).onConflictDoNothing();
    const others = ((yield* keptFor(volumeId, mirror))?.approvals ?? []).filter((kept) => !sameApproval(kept, approval));
    if (others.length >= MAX_KEPT_APPROVALS) return false;
    yield* database.drizzle.update(volumeMirrorStatus).set({ approvals: [approval, ...others], updatedAt: new Date() })
      .where(and(eq(volumeMirrorStatus.volumeId, volumeId), eq(volumeMirrorStatus.machineId, mirror)));
    return true;
  }));
});

/** The Mirror on `mirror` accepted a pull carrying `accepted`: puts it first. A Mirror deleted since keeps none. */
export const acceptApproval = Effect.fn("MirrorApprovals.accept")(function* (volumeId: string, mirror: MachineId, accepted: Signed) {
  const database = yield* Database;
  yield* database.transaction(Effect.gen(function* () {
    const kept = (yield* keptFor(volumeId, mirror))?.approvals ?? null;
    if (kept === null || (kept[0] !== undefined && sameApproval(kept[0], accepted))) return;
    const approvals = [accepted, ...kept.filter((entry) => !sameApproval(entry, accepted))];
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
