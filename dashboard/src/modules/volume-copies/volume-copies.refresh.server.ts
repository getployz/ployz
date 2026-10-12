import "@tanstack/react-start/server-only";
import type { EnvironmentRef } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { type BlockerReader, placementRefusal, readPlacementBlockers } from "#/modules/volume-placement/volume-placement.deploy.server";
import { runWords } from "#/modules/volume-run/volume-copies";
import { ACTIVE_VOLUME_RUN_STATES } from "#/modules/volume-run/volume-run";
import { listVolumeRuns, requestVolumeRun } from "#/modules/volume-run/volume-run.server";
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
  const [row] = yield* drizzle.select({ deploymentId: volumePlacement.deploymentId }).from(volumePlacement)
    .where(and(eq(volumePlacement.organizationId, caller.organizationId), eq(volumePlacement.volumeId, volumeId)));
  if (row?.deploymentId != null) return { ok: false, refusal: busy("A deploy is moving this Volume") } as const;
  const requested = yield* requestVolumeRun(caller, { volumeId, environment, kind: "sync", args: { full: false } });
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
  const rounds = (yield* listVolumeRuns(organizationId, volumeId)).filter((run) => run.kind === "sync" && run.finished_at !== null);
  return { ended: rounds[0] ?? null, done: rounds.find((run) => run.state === "done") ?? null };
});

