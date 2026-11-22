import "@tanstack/react-start/server-only";
import type { CopyObservation } from "@ployz/sdk";
import { Data, Effect } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import { clusterExpectation, loadOrganizationConnections, logForeignServers } from "#/modules/runtime/connections.server";
import { organizationMachine } from "#/modules/machines/tables";
import { observeVolumeCopies } from "#/modules/runtime/ployz.server";
import { type PlanInput, planFromCopies } from "#/modules/volume-run/plan";
import { volumeRun } from "#/modules/volume-run/tables";
import { dockerVolumeName, parseDockerVolumeName } from "#/modules/volume-run/volume-run";
import { Database } from "#/server/database.server";
import type { SecretEncryption } from "#/utils/encrypted-secret.server";
import { observeVolumeMembers } from "#/modules/volume-run/volume-run.cluster.server";
import { enqueueRun } from "#/modules/volume-run/volume-run.rows.server";

/** A staged removal not yet applied still holds its name, so its slot is not an orphan. */
export function orphanSlotNames(copies: CopyObservation["copies"], configNames: ReadonlySet<string>): string[] {
  const written = new Set(copies.filter((copy) => copy.role !== "slot").map((copy) => copy.name));
  const orphans = copies.filter((copy) => copy.role === "slot" && !configNames.has(copy.name) && !written.has(copy.name));
  return [...new Set(orphans.map((copy) => copy.name))].sort();
}

const configVolumeNames = Effect.fn("VolumeRun.configVolumeNames")(function* (organizationId: string) {
  const names = new Set<string>();
  const { namespaces } = yield* readStore(organizationId, { query: "namespaces" });
  for (const owned of namespaces) {
    const { volumes } = yield* readStore(organizationId, {
      query: "volumes",
      environment: { project: owned.project, environment: owned.environment },
    });
    for (const volume of volumes) names.add(dockerVolumeName(owned.namespace, volume.id));
  }
  return names;
});

export type CopyObserver = (organizationId: string) => Effect.Effect<CopyObservation["copies"], unknown, Database | SecretEncryption>;

class ClusterNotPaired extends Data.TaggedError("ClusterNotPaired")<{ readonly organizationId: string }> {}

/** Every copy the Cluster's Servers report, failing when Cloud holds no pairing to ask them through. */
export const observeOrganization = Effect.fn("VolumeRun.observeOrganization")(function* (organizationId: string) {
  const loaded = yield* loadOrganizationConnections(organizationId);
  if (loaded.kind !== "ready") return yield* new ClusterNotPaired({ organizationId });
  if (loaded.connections.length === 0) return { copies: [], unanswered: [] } satisfies CopyObservation;
  const { foreign, ...observed } = yield* observeVolumeCopies(loaded.connections, clusterExpectation(loaded));
  yield* logForeignServers(organizationId, foreign);
  return observed satisfies CopyObservation;
});

export const observeOrganizationCopies: CopyObserver = (organizationId) =>
  observeOrganization(organizationId).pipe(
    Effect.map((observed) => observed.copies),
    Effect.catchTag("ClusterNotPaired", () => Effect.succeed([])),
  );

function unheldRootNames(copies: CopyObservation["copies"], configNames: ReadonlySet<string>): string[] {
  const roots = copies.filter((copy) => copy.role !== "slot" && !configNames.has(copy.name));
  return [...new Set(roots.map((copy) => copy.name))].sort();
}

export type NameInspector = (organizationId: string, dockerVolume: string) => ReturnType<typeof observeVolumeMembers>;

const orphanDelete = (name: string) =>
  ({ kind: "delete_mirror", args: { slot: null, confirmed_name: null }, volumeName: name, orphan: true }) satisfies PlanInput;

export const findOrphans = Effect.fn("VolumeRun.findOrphans")(function* (
  organizationId: string,
  observe: CopyObserver,
  inspect: NameInspector,
) {
  const copies = yield* observe(organizationId);
  if (copies.length === 0) return [];
  const config = yield* configVolumeNames(organizationId);
  const seeds = yield* Effect.forEach(unheldRootNames(copies, config), (name) => inspect(organizationId, name).pipe(
    Effect.map((members) => (planFromCopies(orphanDelete(name), members).ok ? [name] : [])),
    Effect.catch((error) => Effect.logWarning("Orphan inspect failed.", { organizationId, name, error }).pipe(Effect.as([]))),
  ), { concurrency: 4 });
  return [...orphanSlotNames(copies, config), ...seeds.flat()].sort();
});

export const startOrphanDeletes = Effect.fn("VolumeRun.startOrphanDeletes")(function* (
  organizationId: string,
  observe: CopyObserver = observeOrganizationCopies,
  inspect: NameInspector = observeVolumeMembers,
) {
  const database = yield* Database;
  const names = yield* findOrphans(organizationId, observe, inspect);
  const started: string[] = [];
  for (const name of names) {
    const parsed = parseDockerVolumeName(name);
    if (parsed === null) continue;
    const { kind, args, orphan } = orphanDelete(name);
    const row = yield* database.transaction(Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [inserted] = yield* drizzle.insert(volumeRun).values({
      organizationId,
      environmentId: parsed.namespace,
      volumeId: parsed.volumeId,
      volumeName: name,
      dockerVolume: name,
      kind,
      args,
      orphan,
      requestedByUserId: null,
    }).onConflictDoNothing().returning();
      if (inserted !== undefined) yield* enqueueRun(inserted);
      return inserted;
    }));
    if (row === undefined) continue;
    started.push(row.id);
  }
  return started;
});

const NO_RUNS: readonly string[] = [];

export const cleanOrphans = (organizationId: string) =>
  startOrphanDeletes(organizationId).pipe(
    Effect.catch((error) => Effect.logWarning("Orphan cleanup failed.", { organizationId, error }).pipe(Effect.as(NO_RUNS))),
  );

export const organizationsWithMachines = Effect.fn("VolumeRun.organizationsWithMachines")(function* () {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle.selectDistinct({ organizationId: organizationMachine.organizationId }).from(organizationMachine);
  return rows.map((row) => row.organizationId);
});
