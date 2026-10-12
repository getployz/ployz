import "@tanstack/react-start/server-only";
import type { CopyObservation, EnvironmentRef, MachineId } from "@ployz/sdk";
import { and, eq, isNotNull, sql } from "drizzle-orm";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { refusedWith } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { firstRuntimeFrame, type OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { placementBlockers, placementUpgradeMessage } from "#/modules/volume-placement/daemon-release";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { restoreSettled, stagePlacement } from "#/modules/volume-placement/volume-placement.server";
import {
  appliedMirrors, drainPlacement, nextPlacementStep, observePlacement, placementChanges, type PlacementRow,
} from "#/modules/volume-placement/volume-placement";
import { ACTIVE_VOLUME_RUN_STATES, dockerVolumeName, type VolumeRunState } from "#/modules/volume-run/volume-run";
import {
  type CopyObserver, getVolumeRun, inspectVolumeCopies, listActiveVolumeRuns, observeOrganizationCopies, requestVolumeRun,
} from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

/** A run the Deploy waits on. `mirrors` is what the row records once it ends; null for a run the Deploy didn't start. */
export type PlacementWait = { readonly runId: string; readonly volumeId: string; readonly mirrors: readonly MachineId[] | null };

export type PlacementAdvance =
  | { readonly kind: "wait"; readonly wait: PlacementWait }
  | { readonly kind: "done" }
  | { readonly kind: "stopped" }
  | { readonly kind: "failed"; readonly message: string };

export type MemberInspector = typeof inspectVolumeCopies;

const CLOUD = (organizationId: string) => ({ userId: null, organizationId });
const isActive = (state: VolumeRunState) => ACTIVE_VOLUME_RUN_STATES.some((active) => active === state);

const rowView = (row: typeof volumePlacement.$inferSelect): PlacementRow => ({
  volumeId: row.volumeId, environmentId: row.environmentId, mirrors: row.mirrors, staged: row.staged, deploymentId: row.deploymentId,
});

/**
 * The Deploy gives back what it took: the rows stay staged for the next one. A failed Deploy marks them unreached, so
 * Details can say the failure never got to them.
 */
const release = Effect.fn("VolumePlacement.release")(function* (organizationId: string, deploymentId: string, failed = false) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement).set({
    deploymentId: null,
    staged: failed ? sql`${volumePlacement.staged} || '{"unreached":true}'::jsonb` : sql`${volumePlacement.staged}`,
    updatedAt: new Date(),
  }).where(and(eq(volumePlacement.organizationId, organizationId), eq(volumePlacement.deploymentId, deploymentId)));
});

/**
 * The change on Volume `volumeId` failed, so the Deploy drops it and gives back the rest. A restore stays staged: its
 * Volume waits in the Store for the data, and only discarding it drops the row.
 */
const fail = Effect.fn("VolumePlacement.fail")(function* (organizationId: string, deploymentId: string, volumeId: string, message: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement).set({ staged: null, deploymentId: null, updatedAt: new Date() }).where(and(
    eq(volumePlacement.volumeId, volumeId), eq(volumePlacement.deploymentId, deploymentId), sql`${volumePlacement.staged} -> 'seed' is null`,
  ));
  yield* release(organizationId, deploymentId, true);
  return { kind: "failed", message } satisfies PlacementAdvance;
});

/**
 * Deployment `deploymentId` ended without settling its Volumes: they go back to staged for the next Deploy, unreached if it
 * failed. One still in flight keeps them, so a duplicate run stopping can't take them from the live one. A Volume run
 * already going is its own Inngest function and keeps running; the next Deploy waits on it before planning again.
 */
export const releasePlacementClaims = Effect.fn("VolumePlacement.releaseClaims")(function* (organizationId: string, deploymentId: string) {
  const deployment = yield* readStore(organizationId, { query: "deployment", id: deploymentId }).pipe(
    Effect.map((found) => ({ inFlight: found.in_flight, failed: found.status === "failed" })),
    Effect.catchIf(refusedWith("not_found"), () => Effect.succeed({ inFlight: false, failed: false })),
  );
  if (!deployment.inFlight) yield* release(organizationId, deploymentId, deployment.failed);
});

const settle = Effect.fn("VolumePlacement.settle")(function* (volumeId: string, values: Partial<typeof volumePlacement.$inferInsert>) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement).set({ ...values, updatedAt: new Date() }).where(eq(volumePlacement.volumeId, volumeId));
});

/**
 * Settles Volume `volumeId` only while Deploy `deploymentId` still claims it, and says whether it did. A cancelled Deploy
 * gives its claims back while a step may still be running; the next Deploy may already hold the row.
 */
const settleClaimed = Effect.fn("VolumePlacement.settleClaimed")(function* (
  volumeId: string, deploymentId: string, values: Partial<typeof volumePlacement.$inferInsert>,
) {
  const { drizzle } = yield* Database;
  const updated = yield* drizzle.update(volumePlacement).set({ ...values, updatedAt: new Date() })
    .where(and(eq(volumePlacement.volumeId, volumeId), eq(volumePlacement.deploymentId, deploymentId)))
    .returning({ volumeId: volumePlacement.volumeId });
  return updated.length > 0;
});

/**
 * One step of Deploy `deploymentId`'s storage, before its Services: settle the run it waited on, then start the next run
 * the first stamped Volume needs. A Deploy that is no longer queued (cancelled, superseded) stops between runs, and its
 * Volumes stay staged. A run that fails fails the Deploy: its change is dropped, and the Volumes it never reached stay staged.
 */
export const advancePlacement = Effect.fn("VolumePlacement.advance")(function* (
  organizationId: string, deploymentId: string, waited: PlacementWait | null, inspect: MemberInspector = inspectVolumeCopies,
  read: BlockerReader = readPlacementBlockers,
) {
  const deployment = yield* readStore(organizationId, { query: "deployment", id: deploymentId });
  if (deployment.status !== "queued") {
    yield* release(organizationId, deploymentId);
    return { kind: "stopped" } satisfies PlacementAdvance;
  }
  if (waited !== null) {
    const run = yield* getVolumeRun(organizationId, waited.runId);
    if (isActive(run.state)) return { kind: "wait", wait: waited } satisfies PlacementAdvance;
    if (waited.mirrors !== null && run.state !== "done") {
      return yield* fail(organizationId, deploymentId, waited.volumeId, `${run.volume_name}: ${run.message ?? run.state}`);
    }
    // The run this Deploy started has ended, so its mirrors are what the Servers hold, whoever claims the row now.
    if (waited.mirrors !== null) yield* settle(waited.volumeId, { mirrors: [...waited.mirrors] });
  }
  const { drizzle } = yield* Database;
  const stamped = yield* drizzle.select().from(volumePlacement)
    .where(and(eq(volumePlacement.organizationId, organizationId), eq(volumePlacement.deploymentId, deploymentId), isNotNull(volumePlacement.staged)));
  const settled = new Set<string>();
  for (const row of stamped) if (row.staged?.seed !== undefined && (yield* restoreSettled(row.volumeId))) settled.add(row.volumeId);
  const environment: EnvironmentRef = { project: deployment.environment.project, environment: deployment.environment.name };
  const { volumes } = yield* readStore(organizationId, { query: "volumes", environment });
  const listed = (row: { readonly volumeId: string }) => volumes.some((volume) => volume.id === row.volumeId);
  // A settled restore whose Volume is gone was discarded from the Store: nothing is left to restore into. It goes first,
  // so a Server that can't take part fails no Deploy over it.
  const rows: typeof stamped = [];
  for (const row of stamped) {
    if (listed(row) || !settled.has(row.volumeId)) rows.push(row);
    else yield* drizzle.delete(volumePlacement).where(and(eq(volumePlacement.volumeId, row.volumeId), eq(volumePlacement.deploymentId, deploymentId)));
  }
  // A Server that joined on an older daemon since these were staged, or one that isn't answering, can't take part in a run.
  const refusal = rows.length === 0 ? null : yield* placementRefusal(organizationId, read);
  if (refusal !== null) {
    yield* release(organizationId, deploymentId, true);
    return { kind: "failed", message: refusal.message } satisfies PlacementAdvance;
  }
  // A restore copies from its source first: a move staged on the source in the same Deploy may drop the restore point.
  const ordered = [...rows].sort((a, b) => Number(b.staged?.seed !== undefined) - Number(a.staged?.seed !== undefined));
  // Sources of restores whose Volume isn't in the Store yet: a move on one waits, so the restore point stays.
  const waiting = new Set<string>();
  for (const row of ordered) {
    const volume = volumes.find((entry) => entry.id === row.volumeId);
    if (volume === undefined) {
      const seed = row.staged?.seed;
      // A restore still being staged: its Volume can be on its way into the Store; only staging or discarding it drops its row.
      if (seed !== undefined) waiting.add(seed.sourceVolumeId);
      yield* settleClaimed(row.volumeId, deploymentId, seed === undefined ? { staged: null, deploymentId: null } : { deploymentId: null });
      continue;
    }
    if (waiting.has(row.volumeId)) {
      yield* settleClaimed(row.volumeId, deploymentId, { deploymentId: null });
      continue;
    }
    // A restore's run belongs to its source, the Volume it copies from.
    const runsOn = row.staged?.seed?.sourceVolumeId ?? row.volumeId;
    const busy = (yield* listActiveVolumeRuns(organizationId)).find((run) => run.volume_id === runsOn);
    if (busy !== undefined) return { kind: "wait", wait: { runId: busy.id, volumeId: row.volumeId, mirrors: null } } satisfies PlacementAdvance;
    const placement = rowView(row);
    const members = yield* inspect(organizationId, environment, row.volumeId);
    const step = nextPlacementStep(placementChanges(volume, members, placement), appliedMirrors(observePlacement(members), placement));
    if (step === null) {
      // Nothing left to run, so the Servers hold what was staged.
      yield* settleClaimed(row.volumeId, deploymentId, { mirrors: [...(row.staged?.mirrors ?? [])], staged: null, deploymentId: null });
      continue;
    }
    // Touching the row without changing it confirms the claim right before the run starts.
    if (!(yield* settleClaimed(row.volumeId, deploymentId, {}))) return { kind: "stopped" } satisfies PlacementAdvance;
    const requested = yield* requestVolumeRun(CLOUD(organizationId), { volumeId: runsOn, environment, ...step.run });
    if (requested.ok) return { kind: "wait", wait: { runId: requested.run.id, volumeId: row.volumeId, mirrors: step.mirrors } } satisfies PlacementAdvance;
    return yield* fail(organizationId, deploymentId, row.volumeId, `${volume.name}: ${requested.refusal.message}`);
  }
  return { kind: "done" } satisfies PlacementAdvance;
});

/** The names of the Servers with storage that lack the volume verbs. Fails unless every Server answers, since a silent one may run anything. */
export type BlockerReader = (organizationId: string) => Effect.Effect<readonly string[], unknown, OrganizationRuntime>;

export const readPlacementBlockers: BlockerReader = (organizationId) => firstRuntimeFrame(organizationId).pipe(
  Effect.flatMap((frame) => frame === null || frame.incomplete_ids.machines.length > 0 ? Effect.fail("not every Server answered") : Effect.succeed(placementBlockers(frame.machines.map((entry) => ({
    name: entry.machine.name, storage: entry.storage?.state ?? null, capabilities: entry.machine.runtime.capabilities ?? [],
  }))))),
);

export const UNANSWERED: StoreRefusal = { code: "unanswered", message: "Your servers aren't answering. Try again once they are.", details: null };

/** Why the organization can't stage a placement or restore yet, or null when it can. */
export const placementRefusal = Effect.fn("VolumePlacement.refusal")(function* (organizationId: string, read: BlockerReader = readPlacementBlockers) {
  const blockers = yield* read(organizationId).pipe(Effect.catch(() => Effect.succeed(null)));
  if (blockers === null) return UNANSWERED;
  if (blockers.length === 0) return null;
  const refusal: StoreRefusal = { code: "upgrade_required", message: placementUpgradeMessage(blockers), details: { servers: [...blockers] } };
  return refusal;
});

/** `write` runs only once the organization can stage; otherwise the refusal answers in its place. */
export const unlessPlacementRefused = <A, E, R>(organizationId: string, write: Effect.Effect<A, E, R>, read: BlockerReader = readPlacementBlockers) =>
  Effect.gen(function* () {
    const refusal = yield* placementRefusal(organizationId, read);
    if (refusal !== null) return { ok: false, refusal } as const;
    return yield* write;
  });

const NO_COPIES: CopyObservation["copies"] = [];

type EnvironmentVolume = { readonly volumeId: string; readonly name: string; readonly environment: EnvironmentRef; readonly dockerVolume: string };

const environmentVolumes = Effect.fn("VolumePlacement.environmentVolumes")(function* (organizationId: string, onlyEnvironmentId: string | null) {
  const found: EnvironmentVolume[] = [];
  const { namespaces } = yield* readStore(organizationId, { query: "namespaces" });
  for (const owned of namespaces) {
    const environment: EnvironmentRef = { project: owned.project, environment: owned.environment };
    const { environment: summary, volumes } = yield* readStore(organizationId, { query: "volumes", environment });
    if (onlyEnvironmentId !== null && summary.id !== onlyEnvironmentId) continue;
    for (const volume of volumes) {
      found.push({ volumeId: volume.id, name: volume.name, environment, dockerVolume: dockerVolumeName(owned.namespace, volume.id) });
    }
  }
  return found;
});

/**
 * A Volume the mirror rounds refresh, from the copies `held` of it: one on a mirror's Server (the row's mirrors, else every
 * slot, as the CLI makes them), or two primaries, which the sync settles by demoting the one that came back.
 */
export function refreshesMirror(row: { readonly mirrors: readonly MachineId[] | null } | undefined, held: CopyObservation["copies"]) {
  const slots = held.filter((copy) => copy.role === "slot").map((copy) => copy.machine_id);
  const mirrors = row?.mirrors ?? slots;
  return held.filter((copy) => copy.role === "writer").length > 1 || slots.some((id) => mirrors.includes(id));
}

/**
 * Refresh every mirror in the organization (or one Environment): a sync for each Volume with a copy on a mirror's
 * Server, or with two primaries, which the sync settles by demoting the one that came back. A detached copy is never
 * refreshed. A Volume a Deploy is moving, or with a run already going, waits for the next refresh.
 */
export const refreshMirrors = Effect.fn("VolumePlacement.refreshMirrors")(function* (
  organizationId: string, onlyEnvironmentId: string | null, observe: CopyObserver = observeOrganizationCopies,
  read: BlockerReader = readPlacementBlockers,
) {
  const copies = yield* observe(organizationId).pipe(
    Effect.catch((error) => Effect.logWarning("Mirror refresh could not observe the copies.", { organizationId, error }).pipe(Effect.as(NO_COPIES))),
  );
  if (copies.length === 0) return [];
  const refusal = yield* placementRefusal(organizationId, read);
  if (refusal !== null) {
    yield* Effect.logInfo("Mirror refresh skipped.", { organizationId, reason: refusal.message });
    return [];
  }
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumePlacement).where(eq(volumePlacement.organizationId, organizationId));
  const requested: string[] = [];
  for (const volume of yield* environmentVolumes(organizationId, onlyEnvironmentId)) {
    const row = rows.find((entry) => entry.volumeId === volume.volumeId);
    if (row?.deploymentId != null) continue;
    if (!refreshesMirror(row, copies.filter((copy) => copy.name === volume.dockerVolume))) continue;
    const run = yield* requestVolumeRun(CLOUD(organizationId), { volumeId: volume.volumeId, environment: volume.environment, kind: "sync", args: { full: false } });
    if (run.ok) requested.push(run.run.id);
  }
  return requested;
});

/**
 * A Drain of `server`: stage each primary on it off to its mirror, else to the first Server in `accepting` the planner
 * takes. A Volume a Deploy is moving keeps that move. Returns the names of Volumes nowhere takes.
 */
export const stageOffServer = Effect.fn("VolumePlacement.stageOffServer")(function* (
  organizationId: string, server: MachineId, accepting: readonly MachineId[],
  observe: CopyObserver = observeOrganizationCopies, inspect: MemberInspector = inspectVolumeCopies,
) {
  const copies = yield* observe(organizationId);
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumePlacement).where(eq(volumePlacement.organizationId, organizationId));
  const stuck: string[] = [];
  for (const volume of yield* environmentVolumes(organizationId, null)) {
    if (!copies.some((copy) => copy.name === volume.dockerVolume && copy.machine_id === server && copy.role === "writer")) continue;
    const found = rows.find((entry) => entry.volumeId === volume.volumeId);
    if (found?.deploymentId != null) continue;
    const row = found === undefined ? null : rowView(found);
    const members = yield* inspect(organizationId, volume.environment, volume.volumeId);
    const staged = drainPlacement({ id: volume.volumeId, name: volume.name }, members, row, accepting, server);
    if (staged === null) {
      stuck.push(volume.name);
      continue;
    }
    const written = yield* stagePlacement(organizationId, {
      environment: volume.environment, volumeId: volume.volumeId, staged, mirrors: appliedMirrors(observePlacement(members), row),
    });
    if (!written.ok) stuck.push(volume.name);
  }
  return { stuck };
});
