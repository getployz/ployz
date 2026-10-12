import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import type { BatchCommand, ConfigCommand, ConfigCommitted, EnvironmentRef, MachineId } from "@ployz/sdk";
import { and, eq, inArray, isNotNull, isNull, sql } from "drizzle-orm";
import { Deferred, Effect, Fiber, Option, Semaphore } from "effect";
import { callStore, readStore } from "#/modules/config-store/config-store.server";
import { StoreRefused, type StoreRefusal } from "#/modules/config-store/store.contract";
import { MAX_NAME, uniqueName } from "#/modules/config-store/store-services";
import { mountChange } from "#/modules/config-store/store-volumes";
import { failedRestore, volumePlacement } from "#/modules/volume-placement/tables";
import {
  lossByVolume, lossTime, oldCopies, observePlacement, placementChanges, placementLosses, type Placement, type PlacementRow, type Seed,
} from "#/modules/volume-placement/volume-placement";
import { dockerVolumeName, parseDockerVolumeName, type VolumeRunInput } from "#/modules/volume-run/volume-run";
import { insertRun, inspectVolumeCopies, planVolumeRun, resolveVolume } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

type Row = typeof volumePlacement.$inferSelect;

export type PlacementWrite = { readonly ok: true; readonly placement: PlacementRow } | { readonly ok: false; readonly refusal: StoreRefusal };
type PlacementDiscard = { readonly ok: true; readonly placement: PlacementRow | null } | { readonly ok: false; readonly refusal: StoreRefusal };

export const view = (row: Row): PlacementRow => {
  const { volumeId, environmentId, mirrors, deploymentId } = row;
  if (row.staged === null) return { volumeId, environmentId, mirrors, staged: null, unreached: false, deploymentId };
  const { unreached = false, ...staged } = row.staged;
  return { volumeId, environmentId, mirrors, staged, unreached, deploymentId };
};

export const DEPLOYING: StoreRefusal = { code: "conflict", message: "A Deploy is moving this Volume. Wait for it to finish.", details: null };
const UNCHECKED: StoreRefusal = { code: "unavailable", message: "Couldn't check the Deploy that took this Volume. Try again.", details: null };
const RESTORING: StoreRefusal = { code: "conflict", message: "This Volume is a staged restore. Deploy or discard it first.", details: null };
const ELSEWHERE: StoreRefusal = { code: "conflict", message: "This restore belongs to another Environment.", details: null };
const STAGING: StoreRefusal = { code: "conflict", message: "This restore is still being staged. Try again in a minute.", details: null };
const UNANSWERED: StoreRefusal = { code: "unavailable", message: "Your servers aren't answering. Try again once they are.", details: null };

/** Held by a Deploy's admission and by a restore being staged in the Environment: neither sees the other half done. */
const environmentLock = (environmentId: string) => sql`hashtext(${`volume-environment:${environmentId}`})`;

/** Each lock holder keeps a pooled connection while its `body` takes more: one at a time can't drain the pool. */
const holders = Semaphore.makeUnsafe(1);

/**
 * Runs `body` while Environment `environmentId`'s lock is held, on a connection of its own so `body`'s queries stay outside
 * its transaction. `body` can't be interrupted: a Store write it stopped awaiting could still commit after the lock goes.
 */
const whileStaging = <A, E, R>(environmentId: string, body: Effect.Effect<A, E, R>) => holders.withPermits(1)(Effect.gen(function* () {
  const database = yield* Database;
  const locked = yield* Deferred.make<void>();
  const staged = yield* Deferred.make<void>();
  const holder = yield* database.transaction(Effect.gen(function* () {
    yield* (yield* Database).drizzle.execute(sql`select pg_advisory_xact_lock(${environmentLock(environmentId)})`);
    yield* Deferred.succeed(locked, undefined);
    yield* Deferred.await(staged);
  })).pipe(Effect.orDie, Effect.onError((cause) => Deferred.failCause(locked, cause)), Effect.forkChild);
  yield* Deferred.await(locked);
  return yield* Effect.uninterruptible(body).pipe(Effect.ensuring(Deferred.succeed(staged, undefined).pipe(Effect.andThen(Fiber.await(holder)))));
}));

/**
 * Whether no stageRestore holds Environment `environmentId`: each restore's Store write has then landed or never will, so
 * a missing Volume means its restore is gone. Ask before reading the Store, or a write that lands in between reads as
 * missing. The lock is let go in the same statement, so a transaction asking keeps no restore waiting.
 */
export const restoreSettled = Effect.fn("VolumePlacement.restoreSettled")(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.execute<{ free: boolean }>(sql`
    select case when pg_try_advisory_lock(${environmentLock(environmentId)}) then pg_advisory_unlock(${environmentLock(environmentId)}) else false end as free`, "objects");
  return row?.free === true;
});

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
const claimed = Effect.fn("VolumePlacement.claimed")(function* (organizationId: string, row: Pick<Row, "deploymentId"> | undefined) {
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
 * new Volume named `<source>-HHMM` (shortened to fit, suffixed when taken), and the row says the next Deploy copies the
 * point onto the source's primary.
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
  const time = lossTime(new Date(input.at * 1000)).replace(":", "");
  const name = uniqueName(`${listed.name.slice(0, MAX_NAME - time.length - 1)}-${time}`, volumes.map((volume) => volume.name));
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
  return yield* whileStaging(environment.id, Effect.gen(function* () {
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
  }));
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
    const settled = yield* restoreSettled(current.environmentId);
    const read = yield* readStore(organizationId, { query: "volumes", environment });
    if (read.environment.id !== current.environmentId) return { ok: false, refusal: ELSEWHERE } satisfies PlacementDiscard;
    const restored = read.volumes.find((volume) => volume.id === volumeId);
    const source = read.volumes.find((volume) => volume.id === seed.sourceVolumeId);
    // Its Store write may still commit, and needs the row when it does. Once settled, a missing Volume was discarded.
    if (restored === undefined && !settled) return { ok: false, refusal: STAGING } satisfies PlacementDiscard;
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

export const RESTORE_FAILED: StoreRefusal = { code: "conflict", message: "Restore failed. Restore again.", details: null };

/** Whether Deployment `deploymentId`'s revision holds a restored Volume whose copy failed: shipping it again mounts it empty. */
export const holdsFailedRestore = Effect.fn("VolumePlacement.holdsFailedRestore")(function* (organizationId: string, deploymentId: string) {
  const { drizzle } = yield* Database;
  const failed = yield* drizzle.select({ volumeId: failedRestore.volumeId }).from(failedRestore).where(eq(failedRestore.organizationId, organizationId));
  if (failed.length === 0) return false;
  const { nodes } = yield* readStore(organizationId, { query: "deployment", id: deploymentId });
  return nodes.some((node) => node.type === "volume" && failed.some((row) => row.volumeId === node.id));
});

/**
 * Writes `command` through `write`, where every path to the Store meets it: the dashboard, the CLI, MCP and the agent.
 * An admitted Deploy takes the placements staged in its Environment, and only one admit stamps: a Redeploy ships what
 * its Deployment froze, so they wait for the next Deploy. The Deploy is refused `confirmation_required`, as the Store
 * refuses one that deletes Volumes, unless it names each Volume whose copy or writes go, and `unavailable` while the
 * Servers can't be asked what they hold. The Environment's lock is held from that check to the stamp: a restore staged
 * meanwhile waits for the next Deploy whole, and an edit staged meanwhile waits and then finds the Deploy holds its row.
 * The names consumed here leave `accept_volume_loss`, since the Store refuses a name it doesn't delete itself.
 */
export const writeAdmitted = <R>(
  organizationId: string,
  command: ConfigCommand,
  write: (command: ConfigCommand) => Effect.Effect<ConfigCommitted, StoreRefused, R>,
  inspect: typeof inspectVolumeCopies = inspectVolumeCopies,
) => Effect.gen(function* () {
  if (command.command === "admit" && command.admit === "retry") {
    if (yield* holdsFailedRestore(organizationId, command.deployment)) return yield* new StoreRefused(RESTORE_FAILED);
    return yield* write(command);
  }
  if (command.command !== "admit" || command.admit !== "deploy") return yield* write(command);
  const database = yield* Database;
  const { environment } = yield* readStore(organizationId, { query: "volumes", environment: command.environment });
  let written: ConfigCommitted | undefined;
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(${environmentLock(environment.id)})`);
    const rows = yield* drizzle.select().from(volumePlacement)
      .where(and(eq(volumePlacement.organizationId, organizationId), eq(volumePlacement.environmentId, environment.id), isNotNull(volumePlacement.staged)))
      .orderBy(volumePlacement.volumeId)
      .for("update");
    // Read under the lock: a restore staged before it has landed or never will, so a seeded Volume missing is gone.
    const { volumes } = rows.length === 0 ? { volumes: [] } : yield* readStore(organizationId, { query: "volumes", environment: command.environment });
    const listed = rows.flatMap((row) => {
      const volume = volumes.find((entry) => entry.id === row.volumeId);
      return volume === undefined ? [] : [{ row, volume }];
    });
    // Asked together, a silent Server costs the admission one timeout, not one per Volume.
    const members = yield* Effect.forEach(listed, ({ row }) => inspect(organizationId, command.environment, row.volumeId), { concurrency: "unbounded" })
      .pipe(Effect.option);
    if (Option.isNone(members)) return yield* new StoreRefused(UNANSWERED);
    const lost = listed.flatMap(({ row, volume }, index) =>
      placementLosses(placementChanges({ id: volume.id, name: volume.name }, members.value[index] ?? [], view(row))));
    const taken = listed.map(({ row }) => row.volumeId);
    const names = [...new Set(lost.map(({ volume }) => volume.name))];
    const accepted = command.accept_volume_loss ?? [];
    if (!names.every((name) => accepted.includes(name))) {
      const version = command.version ?? (yield* readStore(organizationId, { query: "diff", environment: command.environment })).version;
      return yield* new StoreRefused({
        code: "confirmation_required",
        message: `Deploying loses data of ${names.join(", ")}. Accept each by name`,
        details: {
          volumes: lossByVolume(lost),
          accept: [...new Set([...accepted, ...names])],
          version,
        },
      });
    }
    const deleted = (name: string) => volumes.some((volume) => volume.name === name && volume.change === "delete");
    written = yield* write({ ...command, accept_volume_loss: accepted.filter((name) => !names.includes(name) || deleted(name)) });
    if (written.written === "deployment" && written.status !== "applied") yield* stampPlacements(organizationId, taken, written.id);
    return written;
  })).pipe(
    // The Store write committed: nothing after it may answer as its refusal. The rows stay staged for the next Deploy.
    Effect.catchCause((cause) => written === undefined
      ? Effect.failCause(cause)
      : Effect.logWarning("A committed Deploy didn't take its Volume placements.", cause).pipe(Effect.as(written))),
    Effect.catchTags({ SqlError: Effect.die, DatabasePostCommitFailure: Effect.die }),
  );
}).pipe(Effect.withSpan("VolumePlacement.writeAdmitted"));

/** Deploy `deploymentId` takes the placements staged on `volumeIds`. */
export const stampPlacements = Effect.fn("VolumePlacement.stamp")(function* (organizationId: string, volumeIds: readonly string[], deploymentId: string) {
  if (volumeIds.length === 0) return;
  const { drizzle } = yield* Database;
  yield* drizzle.update(volumePlacement)
    .set({ deploymentId, staged: sql`${volumePlacement.staged} - 'unreached'`, updatedAt: new Date() })
    .where(and(
      eq(volumePlacement.organizationId, organizationId),
      inArray(volumePlacement.volumeId, [...volumeIds]),
      isNotNull(volumePlacement.staged),
    ));
});

const NOT_OLD = { message: "Not an old copy" };

export const deleteOldCopy = Effect.fn("VolumePlacement.deleteOldCopy")(function* (
  organizationId: string,
  userId: string | null,
  input: { readonly volumeId: string; readonly environment: EnvironmentRef; readonly server: string },
  inspect: typeof inspectVolumeCopies = inspectVolumeCopies,
) {
  const row = yield* readRow(organizationId, input.volumeId);
  const busy = yield* claimed(organizationId, row);
  if (busy !== null) return { ok: false, refusal: { message: busy.message } } as const;
  const members = yield* inspect(organizationId, input.environment, input.volumeId);
  const old = oldCopies(observePlacement(members), row === undefined ? null : view(row)).find((copy) => copy.server.name === input.server);
  if (old === undefined) return { ok: false, refusal: NOT_OLD } as const;
  const run = { volumeId: input.volumeId, environment: input.environment, kind: "delete_mirror", args: { slot: input.server, confirmed_name: null } } as const;
  const planned = yield* planVolumeRun(organizationId, run);
  if (!planned.ok) return { ok: false, refusal: { message: planned.refusal.message } } as const;
  const requested = yield* requestUnclaimedRun({ userId, organizationId }, run, old.server.id);
  return requested.ok ? { ok: true, run: requested.run } as const : { ok: false, refusal: { message: requested.refusal.message } } as const;
});

/**
 * Requests `run` unless a Deploy still runs the Volume's placement, or, for an old copy's delete, the placement now keeps
 * the copy on `deletes`. The row lock makes the check and the insert one step against writeAdmitted's stamp and staging: a
 * Deploy stamped after it finds the run going and waits for it.
 */
export const requestUnclaimedRun = Effect.fn("VolumePlacement.requestUnclaimedRun")(function* (
  caller: { readonly userId: string | null; readonly organizationId: string },
  input: { readonly volumeId: string; readonly environment: EnvironmentRef } & Extract<VolumeRunInput, { kind: "sync" | "delete_mirror" }>,
  deletes: MachineId | null = null,
) {
  const database = yield* Database;
  const volume = yield* resolveVolume(caller.organizationId, input.environment, input.volumeId);
  return yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    const [row] = yield* drizzle.select().from(volumePlacement)
      .where(and(eq(volumePlacement.organizationId, caller.organizationId), eq(volumePlacement.volumeId, input.volumeId))).for("update");
    const busy = yield* claimed(caller.organizationId, row);
    if (busy !== null) return { ok: false, refusal: busy } as const;
    const kept = row === undefined ? [] : [...row.mirrors ?? [], ...row.staged === null ? [] : [row.staged.primary, ...row.staged.mirrors]];
    if (deletes !== null && kept.includes(deletes)) return { ok: false, refusal: NOT_OLD } as const;
    return yield* insertRun(caller, input, volume, randomUUID());
  }));
});
