import "@tanstack/react-start/server-only";
import type { EnvironmentRef, SnapshotLabel, VolumeSnapshots, WantedSnapshot } from "@ployz/sdk";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { environmentVolumes } from "#/modules/volume-placement/volume-placement.deploy.server";
import { resolveVolume } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";
import {
  DEFAULT_POLICY, type DeployMoment, type SnapshotPolicy, deploySnapshotId, dueAt, pruneIds, scheduledSnapshotId,
} from "./snapshot-schedule";
import { volumeSnapshotPolicy } from "./tables";
import { type SnapshotWriter, type SnapshotsFailure, type SnapshotsResult, snapshotWriter } from "./volume-snapshots.server";
import { snapshotFromWire } from "./volume-snapshots";

export type SnapshotPass =
  | { readonly status: "done"; readonly taken: string | null; readonly deleted: readonly string[] }
  | { readonly status: "skipped"; readonly reason: SnapshotsFailure; readonly message: string };

const NO_EPOCH: Extract<SnapshotsResult, { ok: false }> = {
  ok: false, reason: "no_writer", refusal: { code: "conflict", message: "The volume has no writer", details: null },
};

const holds = (snapshots: VolumeSnapshots, id: string) => snapshots.snapshots.some((snapshot) => snapshot.id === id);

/**
 * Takes `take` unless the writer already holds it, then deletes what `policy` prunes. Any refusal, of the take or of a
 * delete, skips the pass and logs it; a Mismatch alone reads the epoch again, once per pass, and retries. A delete the
 * writer no longer has counts as done.
 */
export const snapshotAndPrune = Effect.fn("SnapshotSchedule.snapshotAndPrune")(function* <E, R>(
  writer: SnapshotWriter<E, R>,
  policy: SnapshotPolicy,
  take: { readonly id: string; readonly label: SnapshotLabel } | null,
) {
  const listed = yield* writer.list;
  if (!listed.ok) return yield* skipped(listed);
  let current = listed.snapshots;
  let reread = false;
  const setAt = (want: WantedSnapshot) => (current.epoch === null ? Effect.succeed(NO_EPOCH) : writer.set(current.epoch, want));
  const set = (want: WantedSnapshot) => Effect.gen(function* () {
    const first = yield* setAt(want);
    if (first.ok || first.reason !== "mismatch" || reread) return first;
    reread = true;
    const again = yield* writer.list;
    if (!again.ok) return again;
    current = again.snapshots;
    const settled = want.kind === "taken" ? holds(current, want.id) : want.kind === "deleted" && !holds(current, want.id);
    return settled ? again : yield* setAt(want);
  });

  if (take !== null && !holds(current, take.id)) {
    const result = yield* set({ kind: "taken", id: take.id, label: take.label, kept: false });
    if (!result.ok) return yield* skipped(result);
    current = result.snapshots;
  }
  const deleted: string[] = [];
  const newestFirst = current.snapshots.map(snapshotFromWire).sort((a, b) => b.at.getTime() - a.at.getTime());
  for (const id of pruneIds(newestFirst, policy)) {
    const result = yield* set({ kind: "deleted", id });
    if (result.ok) {
      current = result.snapshots;
      deleted.push(id);
    } else if (result.reason !== "not_found") {
      return yield* skipped(result);
    }
  }
  return { status: "done", taken: take?.id ?? null, deleted } satisfies SnapshotPass;
});

const skipped = (result: Extract<SnapshotsResult, { ok: false }>) =>
  Effect.logInfo("Snapshot pass skipped.", { reason: result.reason, message: result.refusal.message }).pipe(
    Effect.as<SnapshotPass>({ status: "skipped", reason: result.reason, message: result.refusal.message }),
  );

export const readSnapshotPolicy = Effect.fn("SnapshotSchedule.readPolicy")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select({ policy: volumeSnapshotPolicy.policy }).from(volumeSnapshotPolicy)
    .where(and(eq(volumeSnapshotPolicy.organizationId, organizationId), eq(volumeSnapshotPolicy.volumeId, volumeId)));
  return row?.policy ?? DEFAULT_POLICY;
});

/** Stores the policy and prunes to it now; the count is what the prune deleted. */
export const saveSnapshotPolicy = Effect.fn("SnapshotSchedule.savePolicy")(function* (
  organizationId: string, environment: EnvironmentRef, volumeId: string, policy: SnapshotPolicy,
) {
  const volume = yield* resolveVolume(organizationId, environment, volumeId);
  const { drizzle } = yield* Database;
  yield* drizzle.insert(volumeSnapshotPolicy)
    .values({ volumeId, organizationId, environmentId: volume.environmentId, policy })
    .onConflictDoUpdate({ target: volumeSnapshotPolicy.volumeId, set: { policy, environmentId: volume.environmentId, updatedAt: new Date() } });
  const pass = yield* snapshotAndPrune(snapshotWriter(organizationId, environment, volumeId), policy, null);
  return { deleted: pass.status === "done" ? pass.deleted.length : 0 };
});

export type ScheduledVolume = { readonly organizationId: string; readonly environment: EnvironmentRef; readonly volumeId: string };

/** The Volumes whose schedule is due on `tick`; an organization whose Store cannot be read waits for the next tick. */
export const scheduledVolumes = Effect.fn("SnapshotSchedule.scheduledVolumes")(function* (tick: Date) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select({ organizationId: volumeSnapshotPolicy.organizationId, volumeId: volumeSnapshotPolicy.volumeId, policy: volumeSnapshotPolicy.policy })
    .from(volumeSnapshotPolicy);
  const due = rows.filter((row) => dueAt(row.policy.every, tick));
  const byOrganization = new Map<string, typeof due>();
  for (const row of due) byOrganization.set(row.organizationId, [...byOrganization.get(row.organizationId) ?? [], row]);
  const found: ScheduledVolume[] = [];
  for (const [organizationId, volumes] of byOrganization) {
    const known = yield* environmentVolumes(organizationId, null).pipe(
      Effect.catch((error) => Effect.logWarning("Snapshot schedule could not read the Store.", { organizationId, error }).pipe(Effect.as([]))),
    );
    for (const row of volumes) {
      const volume = known.find((entry) => entry.volumeId === row.volumeId);
      if (volume !== undefined) found.push({ organizationId, environment: volume.environment, volumeId: row.volumeId });
    }
  }
  return found;
});

/** One Volume's tick: the snapshot its schedule names, if the schedule is still due, then the prune. */
export const snapshotOnTick = Effect.fn("SnapshotSchedule.onTick")(function* (volume: ScheduledVolume, tick: Date) {
  const policy = yield* readSnapshotPolicy(volume.organizationId, volume.volumeId);
  if (policy.every === "off" || !dueAt(policy.every, tick)) return { status: "done", taken: null, deleted: [] } satisfies SnapshotPass;
  const take = { id: scheduledSnapshotId(policy.every, volume.volumeId, tick), label: { kind: policy.every === "hour" ? "hourly" : "daily", note: "" } } as const;
  return yield* snapshotAndPrune(snapshotWriter(volume.organizationId, volume.environment, volume.volumeId), policy, take);
});

/**
 * The deploy's Before or After snapshot of every Volume in the Environment whose policy asks for it. A Volume whose
 * writer refuses is skipped, never the deploy; see `deploySnapshots`.
 */
export const snapshotForDeploy = Effect.fn("SnapshotSchedule.forDeploy")(function* (
  organizationId: string, environmentId: string, deploymentId: string, moment: DeployMoment,
) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumeSnapshotPolicy)
    .where(and(eq(volumeSnapshotPolicy.organizationId, organizationId), eq(volumeSnapshotPolicy.environmentId, environmentId)));
  const asked = rows.filter((row) => (moment === "before" ? row.policy.before : row.policy.after));
  if (asked.length === 0) return [];
  const deployment = yield* readStore(organizationId, { query: "deployment", id: deploymentId });
  const volumes = yield* environmentVolumes(organizationId, environmentId);
  const label = { kind: moment === "before" ? "before_deploy" : "after_deploy", note: `#${deployment.number}` } as const;
  type Outcome = { readonly volumeId: string; readonly status: SnapshotPass["status"] | "gone" | "failed" };
  return yield* Effect.forEach(asked, (row) => {
    const volume = volumes.find((entry) => entry.volumeId === row.volumeId);
    if (volume === undefined) return Effect.succeed<Outcome>({ volumeId: row.volumeId, status: "gone" });
    const take = { id: deploySnapshotId(moment, deploymentId), label };
    return snapshotAndPrune(snapshotWriter(organizationId, volume.environment, volume.volumeId), row.policy, take).pipe(
      Effect.map((pass): Outcome => ({ volumeId: row.volumeId, status: pass.status })),
      Effect.catch((error) => Effect.logWarning("Deploy snapshot failed.", { volumeId: row.volumeId, error }).pipe(Effect.as<Outcome>({ volumeId: row.volumeId, status: "failed" }))),
    );
  });
});

/** Best effort: a snapshot the deploy could not take never fails the deploy. */
export const deploySnapshots = (organizationId: string, environmentId: string, deploymentId: string, moment: DeployMoment) =>
  snapshotForDeploy(organizationId, environmentId, deploymentId, moment).pipe(
    Effect.catch((error) => Effect.logWarning("Deploy snapshots skipped.", { deploymentId, moment, error }).pipe(Effect.as([]))),
  );
