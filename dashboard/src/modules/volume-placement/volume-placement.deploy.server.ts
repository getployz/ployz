import "@tanstack/react-start/server-only";
import type { CopyObservation, EnvironmentRef, MachineId, NodeOutcome } from "@ployz/sdk";
import { and, eq, inArray, isNotNull, sql } from "drizzle-orm";
import { Data, Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { refusedWith } from "#/modules/config-store/store-sdk.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { firstRuntimeFrame, type OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import {
  placementLacking, placementUnknown, placementUnknownMessage, placementUpgradeMessage, type StorageServer,
} from "#/modules/volume-placement/daemon-release";
import { failedRestore, volumePlacement } from "#/modules/volume-placement/tables";
import { discardPlacement, RESTORE_FAILED, restoreSettled, stagePlacement, view } from "#/modules/volume-placement/volume-placement.server";
import {
  appliedMirrors, changeKey, drainPlacement, dropChange, nextPlacementStep, observePlacement, type Placement, placementChanges, type PlacementStep, type PlacementVolume,
  sameChange,
} from "#/modules/volume-placement/volume-placement";
import { ACTIVE_VOLUME_RUN_STATES, dockerVolumeName, type VolumeRunArgs, type VolumeRunState } from "#/modules/volume-run/volume-run";
import {
  type CopyObserver, getVolumeRun, inspectVolumeCopies, listActiveVolumeRuns, observeOrganizationCopies, requestVolumeRun,
} from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

/** A run the Deploy waits on. `step` is the one it started it for; null for a run the Deploy didn't start. */
export type PlacementWait = { readonly runId: string; readonly volumeId: string; readonly step: PlacementStep | null };

export type PlacementAdvance =
  | { readonly kind: "wait"; readonly wait: PlacementWait }
  | { readonly kind: "done" }
  | { readonly kind: "stopped" }
  | { readonly kind: "failed"; readonly message: string; readonly restore?: FailedRestore };

/**
 * A restore whose copy failed. Its row keeps the Deploy's claim: the Store stages its Volume again only once it records
 * the Deploy failed, and Discard then takes back both halves.
 */
export type FailedRestore = { readonly volumeId: string; readonly environment: EnvironmentRef };

export type MemberInspector = typeof inspectVolumeCopies;

const CLOUD = (organizationId: string) => ({ userId: null, organizationId });
const isActive = (state: VolumeRunState) => ACTIVE_VOLUME_RUN_STATES.some((active) => active === state);

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

const fail = Effect.fn("VolumePlacement.fail")(function* (
  organizationId: string, deploymentId: string, volumeId: string, remaining: Placement | null, message: string,
  mirrors: readonly MachineId[] | null = null,
) {
  const { drizzle } = yield* Database;
  const claim = and(eq(volumePlacement.volumeId, volumeId), eq(volumePlacement.deploymentId, deploymentId));
  const settled: Partial<typeof volumePlacement.$inferInsert> = { staged: remaining, updatedAt: new Date() };
  if (mirrors !== null) settled.mirrors = [...mirrors];
  // Kept whole, the failed change landed or handed over: the Deploy reached the row, and a Redeploy finishes it.
  if (remaining !== null) {
    yield* drizzle.update(volumePlacement).set({ ...settled, deploymentId: null })
      .where(and(claim, eq(volumePlacement.staged, remaining)));
  }
  yield* drizzle.update(volumePlacement).set(settled).where(claim);
  yield* release(organizationId, deploymentId, true);
  const failed: PlacementAdvance = { kind: "failed", message };
  return failed;
});

const restoreFailed = (volumeId: string, environment: EnvironmentRef, message: string): PlacementAdvance =>
  ({ kind: "failed", message, restore: { volumeId, environment } });

const remainingAfterFailure = Effect.fn("VolumePlacement.remainingAfterFailure")(function* (
  organizationId: string, deploymentId: string, environment: EnvironmentRef, volumes: readonly PlacementVolume[], volumeId: string,
  step: PlacementStep, inspect: MemberInspector,
) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(volumePlacement)
    .where(and(eq(volumePlacement.volumeId, volumeId), eq(volumePlacement.deploymentId, deploymentId)));
  const volume = volumes.find((entry) => entry.id === volumeId);
  if (row?.staged == null || volume === undefined) return { remaining: null, mirrors: null };
  const members = yield* inspect(organizationId, environment, volumeId);
  const placement = view(row);
  // A change the Servers no longer show landed despite the failure, so the step's mirrors are what they hold, as on success.
  const landed = !placementChanges(volume, members, placement).some((change) => sameChange(changeKey(change), step.change));
  return { remaining: dropChange(volume, members, placement, step.change), mirrors: landed ? step.mirrors : null };
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

/**
 * Discards `restore` once its Deploy has failed. Refused, it stays staged for Discard or the next Deploy. Either way its
 * Volume is marked, so no Redeploy ships the failed Deploy's revision with it empty.
 */
export const discardFailedRestore = Effect.fn("VolumePlacement.discardFailedRestore")(function* (organizationId: string, restore: FailedRestore) {
  const { drizzle } = yield* Database;
  yield* drizzle.insert(failedRestore).values({ volumeId: restore.volumeId, organizationId }).onConflictDoNothing();
  const refusal = yield* discardPlacement(organizationId, null, restore.volumeId, restore.environment).pipe(
    Effect.map((discarded) => discarded.ok ? null : discarded.refusal.message),
    Effect.catchTag("StoreRefused", (refused) => Effect.succeed(refused.message)),
  );
  if (refusal !== null) yield* Effect.logWarning(`A failed restore stays staged: ${refusal}`);
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
 * Why Deploy `deploymentId` can't start, if its revision holds a restored Volume it would create empty: one whose restore
 * this Deploy didn't take, or whose copy already failed. The failed one comes back to be taken back again.
 */
const unseededVolume = Effect.fn("VolumePlacement.unseededVolume")(function* (
  organizationId: string, deploymentId: string, nodes: readonly NodeOutcome[], environment: EnvironmentRef,
) {
  const held = nodes.flatMap((node) => node.type === "volume" ? [node] : []);
  if (held.length === 0) return null;
  const ids = held.map((node) => node.id);
  const { drizzle } = yield* Database;
  const failed = yield* drizzle.select({ volumeId: failedRestore.volumeId }).from(failedRestore)
    .where(and(eq(failedRestore.organizationId, organizationId), inArray(failedRestore.volumeId, ids)));
  // Waits on an admission still holding the rows, so a stamp it is about to commit is seen. Locked in admission's order.
  const unstamped = yield* drizzle.select({ volumeId: volumePlacement.volumeId }).from(volumePlacement).where(and(
    eq(volumePlacement.organizationId, organizationId), inArray(volumePlacement.volumeId, ids),
    sql`${volumePlacement.staged} ? 'seed' and ${volumePlacement.deploymentId} is distinct from ${deploymentId}`,
  )).orderBy(volumePlacement.volumeId).for("share");
  const name = (volumeId: string) => held.find((node) => node.id === volumeId)?.name ?? volumeId;
  if (failed[0] !== undefined) return restoreFailed(failed[0].volumeId, environment, `${name(failed[0].volumeId)}: ${RESTORE_FAILED.message}`);
  if (unstamped[0] !== undefined) return { kind: "failed", message: `${name(unstamped[0].volumeId)}: the restore waits for the next Deploy.` } satisfies PlacementAdvance;
  return null;
});

/**
 * The restore that saved its data as `save` is done, so no later Deploy runs it again. Keyed by `save`, since a restore
 * staged afterwards names its own.
 */
const restoreDone = Effect.fn("VolumePlacement.restoreDone")(function* (volumeId: string, save: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement).set({ staged: sql`${volumePlacement.staged} - 'restore'`, updatedAt: new Date() })
    .where(and(eq(volumePlacement.volumeId, volumeId), sql`${volumePlacement.staged} -> 'restore' ->> 'save' = ${save}`));
});

/**
 * One step of Deploy `deploymentId`'s storage, before its Services: settle the run it waited on, then start the next run
 * the first stamped Volume needs. A Deploy that is no longer queued (cancelled, superseded) stops between runs, and its
 * Volumes stay staged. A run that fails fails the Deploy: its change is dropped, and the changes it never reached, on that
 * Volume and the others, stay staged. A failed restore is dropped once the Store records the failure.
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
  const environment: EnvironmentRef = { project: deployment.environment.project, environment: deployment.environment.name };
  const { volumes } = yield* readStore(organizationId, { query: "volumes", environment });
  const unseeded = yield* unseededVolume(organizationId, deploymentId, deployment.nodes, environment);
  if (unseeded !== null) {
    yield* release(organizationId, deploymentId, true);
    return unseeded;
  }
  if (waited !== null) {
    const run = yield* getVolumeRun(organizationId, waited.runId);
    if (isActive(run.state)) return { kind: "wait", wait: waited } satisfies PlacementAdvance;
    if (waited.step !== null && run.state !== "done") {
      const message = `${run.volume_name}: ${run.message ?? run.state}`;
      if (waited.step.change.kind === "seed") return restoreFailed(waited.volumeId, environment, message);
      const { remaining, mirrors } = yield* remainingAfterFailure(organizationId, deploymentId, environment, volumes, waited.volumeId, waited.step, inspect);
      return yield* fail(organizationId, deploymentId, waited.volumeId, remaining, message, mirrors);
    }
    // The run this Deploy started has ended, so its mirrors are what the Servers hold, whoever claims the row now.
    if (waited.step !== null) yield* settle(waited.volumeId, { mirrors: [...waited.step.mirrors] });
    if (run.kind === "restore_snapshot" && run.state === "done") {
      // SAFETY: a run's args are its kind's, and this run's kind is restore_snapshot.
      yield* restoreDone(waited.volumeId, (run.args as VolumeRunArgs["restore_snapshot"]).save);
    }
  }
  const { drizzle } = yield* Database;
  const stamped = yield* drizzle.select().from(volumePlacement)
    .where(and(eq(volumePlacement.organizationId, organizationId), eq(volumePlacement.deploymentId, deploymentId), isNotNull(volumePlacement.staged)));
  const settled = new Set<string>();
  for (const row of stamped) if (row.staged?.seed !== undefined && (yield* restoreSettled(row.environmentId))) settled.add(row.volumeId);
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
    if (busy !== undefined) return { kind: "wait", wait: { runId: busy.id, volumeId: row.volumeId, step: null } } satisfies PlacementAdvance;
    const placement = view(row);
    const members = yield* inspect(organizationId, environment, row.volumeId);
    const changes = placementChanges(volume, members, placement);
    const step = nextPlacementStep(changes, appliedMirrors(observePlacement(members), placement), deployment.number);
    if (step === null) {
      // Nothing left to run, so the Servers hold what was staged.
      yield* settleClaimed(row.volumeId, deploymentId, { mirrors: [...(row.staged?.mirrors ?? [])], staged: null, deploymentId: null });
      continue;
    }
    // Touching the row without changing it confirms the claim right before the run starts.
    if (!(yield* settleClaimed(row.volumeId, deploymentId, {}))) return { kind: "stopped" } satisfies PlacementAdvance;
    const requested = yield* requestVolumeRun(CLOUD(organizationId), { volumeId: runsOn, environment, ...step.run });
    if (requested.ok) return { kind: "wait", wait: { runId: requested.run.id, volumeId: row.volumeId, step } } satisfies PlacementAdvance;
    if (step.change.kind === "seed") return restoreFailed(row.volumeId, environment, `${volume.name}: ${requested.refusal.message}`);
    const remaining = dropChange(volume, members, placement, step.change);
    return yield* fail(organizationId, deploymentId, row.volumeId, remaining, `${volume.name}: ${requested.refusal.message}`);
  }
  return { kind: "done" } satisfies PlacementAdvance;
});

/** Servers with storage whose records say neither their capabilities nor a readable version: placement stays closed, but no Upgrade is claimed. */
export class SupportUnknown extends Data.TaggedError("SupportUnknown")<{ readonly servers: readonly string[] }> {}

/**
 * The names of the Servers with storage that lack the volume verbs. Fails unless every Server answers, since a silent one
 * may run anything, and with SupportUnknown when a Server's record cannot say.
 */
export type BlockerReader = (organizationId: string) => Effect.Effect<readonly string[], unknown, OrganizationRuntime>;

const lackingOrUnknown = (servers: readonly StorageServer[]): Effect.Effect<readonly string[], SupportUnknown> => {
  const lacking = placementLacking(servers);
  const unknown = placementUnknown(servers);
  return lacking.length === 0 && unknown.length > 0 ? Effect.fail(new SupportUnknown({ servers: unknown })) : Effect.succeed(lacking);
};

export const readPlacementBlockers: BlockerReader = (organizationId) => firstRuntimeFrame(organizationId).pipe(
  Effect.flatMap((frame): Effect.Effect<readonly string[], SupportUnknown | string> => frame === null || frame.incomplete_ids.machines.length > 0 ? Effect.fail("not every Server answered") : lackingOrUnknown(frame.machines.map((entry) => ({
    name: entry.machine.name, storage: entry.storage?.state ?? null, daemonVersion: entry.machine.runtime.daemon_version, capabilities: entry.machine.runtime.capabilities ?? [],
  })))),
);

export const UNANSWERED: StoreRefusal = { code: "unanswered", message: "Your servers aren't answering. Try again once they are.", details: null };

/** Why the organization can't stage a placement or restore yet, or null when it can. */
export const placementRefusal = Effect.fn("VolumePlacement.refusal")(function* (organizationId: string, read: BlockerReader = readPlacementBlockers) {
  const blockers = yield* read(organizationId).pipe(Effect.catch((error) => Effect.succeed(error instanceof SupportUnknown ? error : null)));
  if (blockers === null) return UNANSWERED;
  if (blockers instanceof SupportUnknown) {
    const refusal: StoreRefusal = { code: "support_unknown", message: placementUnknownMessage(blockers.servers), details: { servers: [...blockers.servers] } };
    return refusal;
  }
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

export const environmentVolumes = Effect.fn("VolumePlacement.environmentVolumes")(function* (organizationId: string, onlyEnvironmentId: string | null) {
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
    const row = found === undefined ? null : view(found);
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
