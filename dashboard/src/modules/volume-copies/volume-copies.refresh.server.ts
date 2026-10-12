import "@tanstack/react-start/server-only";
import type { EnvironmentRef } from "@ployz/sdk";
import { and, desc, eq, isNotNull, type SQL } from "drizzle-orm";
import { Effect } from "effect";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { type BlockerReader, placementRefusal, readPlacementBlockers } from "#/modules/volume-placement/volume-placement.deploy.server";
import { requestUnclaimedRun } from "#/modules/volume-placement/volume-placement.server";
import { volumeRun } from "#/modules/volume-run/tables";
import { runWords } from "#/modules/volume-run/volume-copies";
import { ACTIVE_VOLUME_RUN_STATES } from "#/modules/volume-run/volume-run";
import { listVolumeRuns, volumeRunView } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

const busy = (message: string): StoreRefusal => ({ code: "conflict", message, details: null });

/**
 * Refresh now: one mirror round for the Volume, the same sync the 15-minute cron requests. A run already going on the
 * Volume answers busy instead of queueing a second, and a Volume a Deploy is moving stays that Deploy's.
 */
export const refreshMirror = Effect.fn("VolumeCopies.refreshMirror")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  environment: EnvironmentRef,
  volumeId: string,
  read: BlockerReader = readPlacementBlockers,
) {
  const refusal = yield* placementRefusal(caller.organizationId, read);
  if (refusal !== null) return { ok: false, refusal } as const;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ mirrors: volumePlacement.mirrors }).from(volumePlacement)
    .where(and(eq(volumePlacement.organizationId, caller.organizationId), eq(volumePlacement.volumeId, volumeId)));
  if (!row?.mirrors?.length) return { ok: false, refusal: busy("No mirror") } as const;
  const requested = yield* requestUnclaimedRun(caller, { volumeId, environment, kind: "sync", args: { full: false } });
  if (requested.ok) return requested;
  const runs = yield* listVolumeRuns(caller.organizationId, volumeId);
  const active = runs.find((run) => ACTIVE_VOLUME_RUN_STATES.some((state) => state === run.state));
  if (active === undefined) return requested;
  return { ok: false, refusal: busy(active.kind === "sync" ? "Already refreshing" : `Busy. ${runWords(active)}.`) } as const;
});

/**
 * The Volume's newest mirror round that ended, whatever its end, for the mirror's skipped state, and the newest that
 * ended done, which ages a mirror whose Server stopped answering.
 */
export const lastMirrorRefresh = Effect.fn("VolumeCopies.lastMirrorRefresh")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const newest = (filter: SQL) => Effect.map(
    drizzle.select().from(volumeRun)
      .where(and(eq(volumeRun.organizationId, organizationId), eq(volumeRun.volumeId, volumeId), eq(volumeRun.kind, "sync"), filter))
      .orderBy(desc(volumeRun.createdAt), desc(volumeRun.id)).limit(1),
    ([row]) => row === undefined ? null : volumeRunView(row),
  );
  return yield* Effect.all({ ended: newest(isNotNull(volumeRun.finishedAt)), done: newest(eq(volumeRun.state, "done")) });
});
