import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import type { BatchCommand, EnvironmentRef, MachineId } from "@ployz/sdk";
import { and, eq, isNotNull, isNull } from "drizzle-orm";
import { Effect } from "effect";
import { callStore, readStore } from "#/modules/config-store/config-store.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { mountChange } from "#/modules/config-store/store-volumes";
import { volumePlacement } from "#/modules/volume-placement/tables";
import { lossTime, observePlacement, type Placement, type PlacementRow, type Seed } from "#/modules/volume-placement/volume-placement";
import { dockerVolumeName, parseDockerVolumeName } from "#/modules/volume-run/volume-run";
import { inspectVolumeCopies, resolveVolume } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

type Row = typeof volumePlacement.$inferSelect;

export type PlacementWrite = { readonly ok: true; readonly placement: PlacementRow } | { readonly ok: false; readonly refusal: StoreRefusal };
type PlacementDiscard = { readonly ok: true; readonly placement: PlacementRow | null } | { readonly ok: false; readonly refusal: StoreRefusal };

const view = (row: Row): PlacementRow => ({
  volumeId: row.volumeId,
  environmentId: row.environmentId,
  mirrors: row.mirrors,
  staged: row.staged,
  deploymentId: row.deploymentId,
});

const DEPLOYING: StoreRefusal = { code: "conflict", message: "A Deploy is moving this Volume. Wait for it to finish.", details: null };
const UNCHECKED: StoreRefusal = { code: "unavailable", message: "Couldn't check the Deploy that took this Volume. Try again.", details: null };
const RESTORING: StoreRefusal = { code: "conflict", message: "This Volume is a staged restore. Deploy or discard it first.", details: null };
const ELSEWHERE: StoreRefusal = { code: "conflict", message: "This restore belongs to another Environment.", details: null };
const STAGING: StoreRefusal = { code: "conflict", message: "This restore is still being staged. Try again in a minute.", details: null };

/** Every Volume placement in the organization: one row per Volume the dashboard has a record for. */
export const listPlacements = Effect.fn("VolumePlacement.list")(function* (organizationId: string) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.select().from(volumePlacement).where(eq(volumePlacement.organizationId, organizationId));
  return rows.map(view);
});

const readRow = Effect.fn("VolumePlacement.readRow")(function* (organizationId: string, volumeId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.select().from(volumePlacement)
    .where(and(eq(volumePlacement.organizationId, organizationId), eq(volumePlacement.volumeId, volumeId)));
  return row;
});

/** Why the row can't change: the Deploy that took it still runs it, or the Store can't say whether it does. */
const claimed = Effect.fn("VolumePlacement.claimed")(function* (organizationId: string, row: Row | undefined) {
  if (row === undefined || row.deploymentId === null) return null;
  return yield* readStore(organizationId, { query: "deployment", id: row.deploymentId }).pipe(
    Effect.map((deployment) => deployment.in_flight ? DEPLOYING : null),
    Effect.catchTag("StoreRefused", () => Effect.succeed(UNCHECKED)),
  );
});

/** The row still has the claim it was checked with: a Deploy admitted since then stamped it, and keeps it. */
const unclaimedSince = (row: Row | undefined) =>
  row?.deploymentId == null ? isNull(volumePlacement.deploymentId) : eq(volumePlacement.deploymentId, row.deploymentId);

/**
 * Stage `staged` (null: nothing) as the Volume's placement for the next Deploy. The first write records `mirrors`, the
 * copies the user sees as mirrors today, since no earlier record says otherwise. Refused while a Deploy runs the last one.
 */
export const stagePlacement = Effect.fn("VolumePlacement.stage")(function* (
  organizationId: string,
  input: { readonly environment: EnvironmentRef; readonly volumeId: string; readonly staged: Placement | null; readonly mirrors: readonly MachineId[] },
) {
  const current = yield* readRow(organizationId, input.volumeId);
  const refusal = yield* claimed(organizationId, current);
  if (refusal !== null) return { ok: false, refusal } satisfies PlacementWrite;
  if (current?.staged?.seed !== undefined) return { ok: false, refusal: RESTORING } satisfies PlacementWrite;
  const { environmentId } = yield* resolveVolume(organizationId, input.environment, input.volumeId);
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.insert(volumePlacement).values({
    volumeId: input.volumeId,
    organizationId,
    environmentId,
    mirrors: [...input.mirrors],
    staged: input.staged,
  }).onConflictDoUpdate({
    target: volumePlacement.volumeId,
    set: { staged: input.staged, deploymentId: null, updatedAt: new Date() },
    setWhere: and(eq(volumePlacement.organizationId, organizationId), unclaimedSince(current)),
  }).returning();
  if (row === undefined) {
    if ((yield* readRow(organizationId, input.volumeId)) !== undefined) return { ok: false, refusal: DEPLOYING } satisfies PlacementWrite;
    return { ok: false, refusal: { code: "conflict", message: "This Volume belongs to another organization.", details: null } } satisfies PlacementWrite;
  }
  return { ok: true, placement: view(row) } satisfies PlacementWrite;
});

/**
 * Stage a new Volume restored from one restore point of `volumeId`: in one Store write, the source's mounts move to a
 * new Volume named `<source>-HHMM`, and the row says the next Deploy copies the point onto the source's primary.
 */
export const stageRestore = Effect.fn("VolumePlacement.stageRestore")(function* (
  organizationId: string,
  userId: string | null,
  input: { readonly volumeId: string; readonly environment: EnvironmentRef; readonly guid: string; readonly name: string; readonly at: number },
  inspect: typeof inspectVolumeCopies = inspectVolumeCopies,
) {
  const source = yield* resolveVolume(organizationId, input.environment, input.volumeId);
  const primary = observePlacement(yield* inspect(organizationId, input.environment, input.volumeId)).primary;
  if (primary === null) {
    return { ok: false, refusal: { code: "conflict", message: `${source.volumeName} has no primary answering to restore from.`, details: null } } satisfies PlacementWrite;
  }
  const { environment, volumes } = yield* readStore(organizationId, { query: "volumes", environment: input.environment });
  const listed = volumes.find((volume) => volume.id === input.volumeId);
  const namespace = parseDockerVolumeName(source.dockerVolume)?.namespace;
  if (listed === undefined || namespace === undefined) return yield* Effect.die(`${source.volumeName} vanished from its Environment`);
  const id = randomUUID();
  const name = `${listed.name}-${lossTime(new Date(input.at * 1000)).replace(":", "")}`;
  const commands: BatchCommand[] = [
    ...listed.mounts.map((mount): BatchCommand => ({ command: "edit", environment: input.environment, expect: null, changes: [mountChange(mount.service, listed.name, null)] })),
    {
      command: "create_volume", id, environment: input.environment, name,
      storage: listed.storage, storage_locked: true, shared_writes: listed.shared_writes, mounts: listed.mounts,
    },
  ];
  const seed: Seed = {
    sourceVolumeId: input.volumeId,
    sourceName: listed.name,
    dockerVolume: dockerVolumeName(namespace, id),
    point: { guid: input.guid, name: input.name, at: input.at },
  };
  // The row lands first: a Deploy admitted right after the batch commits must find it to stamp.
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.insert(volumePlacement).values({
    volumeId: id,
    organizationId,
    environmentId: environment.id,
    mirrors: [],
    staged: { primary: primary.id, mirrors: [], deletes: [], seed },
  }).returning();
  if (row === undefined) return yield* Effect.die("the restore's row was not written");
  const written = yield* callStore(organizationId, userId, {
    operation: "write", command: { command: "batch", environment: input.environment, commands, expect: environment.revision },
  });
  if (!written.ok) {
    yield* drizzle.delete(volumePlacement).where(eq(volumePlacement.volumeId, id));
    return { ok: false, refusal: written.refusal } satisfies PlacementWrite;
  }
  return { ok: true, placement: view(row) } satisfies PlacementWrite;
});

/**
 * Drop what the Volume has staged. Refused while a Deploy runs it. A staged restore also takes back its Store half while
 * it is still staged: the new Volume, and the source's mounts it took over.
 */
export const discardPlacement = Effect.fn("VolumePlacement.discard")(function* (
  organizationId: string, userId: string | null, volumeId: string, environment: EnvironmentRef,
) {
  const current = yield* readRow(organizationId, volumeId);
  if (current === undefined) return { ok: true, placement: null } satisfies PlacementDiscard;
  const refusal = yield* claimed(organizationId, current);
  if (refusal !== null) return { ok: false, refusal } satisfies PlacementDiscard;
  const unchanged = and(eq(volumePlacement.volumeId, current.volumeId), unclaimedSince(current));
  const { drizzle } = yield* Database;
  const seed = current.staged?.seed;
  if (seed !== undefined) {
    const read = yield* readStore(organizationId, { query: "volumes", environment });
    if (read.environment.id !== current.environmentId) return { ok: false, refusal: ELSEWHERE } satisfies PlacementDiscard;
    const restored = read.volumes.find((volume) => volume.id === volumeId);
    const source = read.volumes.find((volume) => volume.id === seed.sourceVolumeId);
    // Its Store write may still commit, and needs the row when it does. No page offers a row without its Volume.
    if (restored === undefined) return { ok: false, refusal: STAGING } satisfies PlacementDiscard;
    const commands: BatchCommand[] = restored?.change !== "create" ? [] : [
      { command: "discard", environment, path: `volumes.${restored.name}`, version: null },
      ...restored.mounts
        .filter((mount) => source !== undefined && !source.mounts.some((held) => held.service === mount.service))
        .map((mount): BatchCommand => ({ command: "discard", environment, path: `${mount.service}.mounts.${seed.sourceName}`, version: null })),
    ];
    if (commands.length > 0) {
      const written = yield* callStore(organizationId, userId, { operation: "write", command: { command: "batch", environment, commands } });
      if (!written.ok) return { ok: false, refusal: written.refusal } satisfies PlacementDiscard;
    }
    const [deleted] = yield* drizzle.delete(volumePlacement).where(unchanged).returning();
    if (deleted === undefined) return { ok: false, refusal: DEPLOYING } satisfies PlacementDiscard;
    return { ok: true, placement: null } satisfies PlacementDiscard;
  }
  const [row] = yield* drizzle.update(volumePlacement)
    .set({ staged: null, deploymentId: null, updatedAt: new Date() })
    .where(unchanged)
    .returning();
  if (row === undefined) return { ok: false, refusal: DEPLOYING } satisfies PlacementDiscard;
  return { ok: true, placement: view(row) } satisfies PlacementDiscard;
});

/** Deploy `deploymentId` takes every placement staged in its Environment. */
export const stampPlacements = Effect.fn("VolumePlacement.stamp")(function* (organizationId: string, environmentId: string, deploymentId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement)
    .set({ deploymentId, updatedAt: new Date() })
    .where(and(
      eq(volumePlacement.organizationId, organizationId),
      eq(volumePlacement.environmentId, environmentId),
      isNotNull(volumePlacement.staged),
    ));
});
