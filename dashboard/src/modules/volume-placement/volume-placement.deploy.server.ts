import "@tanstack/react-start/server-only";
import type { CopyObservation, EnvironmentRef, MachineId } from "@ployz/sdk";
import { and, eq, isNotNull } from "drizzle-orm";
import { Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { appliedMirrors, nextPlacementStep, observePlacement, placementChanges, type PlacementRow } from "#/modules/volume-placement/volume-placement";
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

/** The Deploy gives back what it took: the rows stay staged for the next one. */
const release = Effect.fn("VolumePlacement.release")(function* (organizationId: string, deploymentId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement).set({ deploymentId: null, updatedAt: new Date() })
    .where(and(eq(volumePlacement.organizationId, organizationId), eq(volumePlacement.deploymentId, deploymentId)));
});

const settle = Effect.fn("VolumePlacement.settle")(function* (volumeId: string, values: Partial<typeof volumePlacement.$inferInsert>) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement).set({ ...values, updatedAt: new Date() }).where(eq(volumePlacement.volumeId, volumeId));
});

/**
 * One step of Deploy `deploymentId`'s storage, before its Services: settle the run it waited on, then start the next run
 * the first stamped Volume needs. A Deploy that is no longer queued (cancelled, superseded) stops between runs, and its
 * Volumes stay staged. A run that fails fails the Deploy, its Volumes still staged.
 */
export const advancePlacement = Effect.fn("VolumePlacement.advance")(function* (
  organizationId: string, deploymentId: string, waited: PlacementWait | null, inspect: MemberInspector = inspectVolumeCopies,
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
      yield* release(organizationId, deploymentId);
      return { kind: "failed", message: `${run.volume_name}: ${run.message ?? run.state}` } satisfies PlacementAdvance;
    }
    if (waited.mirrors !== null) yield* settle(waited.volumeId, { mirrors: [...waited.mirrors] });
  }
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumePlacement)
    .where(and(eq(volumePlacement.organizationId, organizationId), eq(volumePlacement.deploymentId, deploymentId), isNotNull(volumePlacement.staged)));
  const environment: EnvironmentRef = { project: deployment.environment.project, environment: deployment.environment.name };
  const { volumes } = yield* readStore(organizationId, { query: "volumes", environment });
  for (const row of rows) {
    const volume = volumes.find((entry) => entry.id === row.volumeId);
    if (volume === undefined) {
      yield* settle(row.volumeId, { staged: null, deploymentId: null });
      continue;
    }
    const busy = (yield* listActiveVolumeRuns(organizationId)).find((run) => run.volume_id === row.volumeId);
    if (busy !== undefined) return { kind: "wait", wait: { runId: busy.id, volumeId: row.volumeId, mirrors: null } } satisfies PlacementAdvance;
    const placement = rowView(row);
    const members = yield* inspect(organizationId, environment, row.volumeId);
    const step = nextPlacementStep(placementChanges(volume, members, placement), appliedMirrors(observePlacement(members), placement));
    if (step === null) {
      yield* settle(row.volumeId, { staged: null, deploymentId: null });
      continue;
    }
    const requested = yield* requestVolumeRun(CLOUD(organizationId), { volumeId: row.volumeId, environment, ...step.run });
    if (requested.ok) return { kind: "wait", wait: { runId: requested.run.id, volumeId: row.volumeId, mirrors: step.mirrors } } satisfies PlacementAdvance;
    yield* release(organizationId, deploymentId);
    return { kind: "failed", message: `${volume.name}: ${requested.refusal.message}` } satisfies PlacementAdvance;
  }
  return { kind: "done" } satisfies PlacementAdvance;
});

const NO_COPIES: CopyObservation["copies"] = [];

type EnvironmentVolume = { readonly volumeId: string; readonly environment: EnvironmentRef; readonly dockerVolume: string };

const environmentVolumes = Effect.fn("VolumePlacement.environmentVolumes")(function* (organizationId: string, onlyEnvironmentId: string | null) {
  const found: EnvironmentVolume[] = [];
  const { namespaces } = yield* readStore(organizationId, { query: "namespaces" });
  for (const owned of namespaces) {
    const environment: EnvironmentRef = { project: owned.project, environment: owned.environment };
    const { environment: summary, volumes } = yield* readStore(organizationId, { query: "volumes", environment });
    if (onlyEnvironmentId !== null && summary.id !== onlyEnvironmentId) continue;
    for (const volume of volumes) {
      found.push({ volumeId: volume.id, environment, dockerVolume: dockerVolumeName(owned.namespace, volume.id) });
    }
  }
  return found;
});

/**
 * Refresh every mirror in the organization (or one Environment): a sync for each Volume with a copy on a mirror's
 * Server, or with two primaries, which the sync settles by demoting the one that came back. A detached copy is never
 * refreshed. A Volume a Deploy is moving, or with a run already going, waits for the next refresh.
 */
export const refreshMirrors = Effect.fn("VolumePlacement.refreshMirrors")(function* (
  organizationId: string, onlyEnvironmentId: string | null, observe: CopyObserver = observeOrganizationCopies,
) {
  const copies = yield* observe(organizationId).pipe(
    Effect.catch((error) => Effect.logWarning("Mirror refresh could not observe the copies.", { organizationId, error }).pipe(Effect.as(NO_COPIES))),
  );
  if (copies.length === 0) return [];
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumePlacement).where(eq(volumePlacement.organizationId, organizationId));
  const requested: string[] = [];
  for (const volume of yield* environmentVolumes(organizationId, onlyEnvironmentId)) {
    const held = copies.filter((copy) => copy.name === volume.dockerVolume);
    const slots = held.filter((copy) => copy.role === "slot").map((copy) => copy.machine_id);
    const row = rows.find((entry) => entry.volumeId === volume.volumeId);
    if (row?.deploymentId != null) continue;
    const mirrors = row?.mirrors ?? slots;
    const twoPrimaries = held.filter((copy) => copy.role === "writer").length > 1;
    if (!twoPrimaries && !slots.some((id) => mirrors.includes(id))) continue;
    const run = yield* requestVolumeRun(CLOUD(organizationId), { volumeId: volume.volumeId, environment: volume.environment, kind: "sync", args: { full: false } });
    if (run.ok) requested.push(run.run.id);
  }
  return requested;
});
