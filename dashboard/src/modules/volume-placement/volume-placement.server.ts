import "@tanstack/react-start/server-only";
import type { EnvironmentRef, MachineId } from "@ployz/sdk";
import { and, eq, isNotNull } from "drizzle-orm";
import { Effect, Option } from "effect";
import { readStore } from "#/modules/config-store/config-store.server";
import type { StoreRefusal } from "#/modules/config-store/store.contract";
import { volumePlacement } from "#/modules/volume-placement/tables";
import type { Placement, PlacementRow } from "#/modules/volume-placement/volume-placement";
import { resolveVolume } from "#/modules/volume-run/volume-run.server";
import { Database } from "#/server/database.server";

type Row = typeof volumePlacement.$inferSelect;

export type PlacementWrite = { readonly ok: true; readonly placement: PlacementRow } | { readonly ok: false; readonly refusal: StoreRefusal };

const view = (row: Row): PlacementRow => ({
  volumeId: row.volumeId,
  environmentId: row.environmentId,
  mirrors: row.mirrors,
  staged: row.staged,
  deploymentId: row.deploymentId,
});

const DEPLOYING: StoreRefusal = { code: "conflict", message: "A Deploy is moving this Volume. Wait for it to finish.", details: null };

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

/** Whether the Deploy that took this row's staged placement is still running it. An ended or unreadable one is not. */
const deploying = Effect.fn("VolumePlacement.deploying")(function* (organizationId: string, row: Row | undefined) {
  if (row === undefined || row.deploymentId === null) return false;
  const found = yield* readStore(organizationId, { query: "deployment", id: row.deploymentId }).pipe(Effect.option);
  return Option.isSome(found) && found.value.in_flight;
});

/**
 * Stage `staged` (null: nothing) as the Volume's placement for the next Deploy. The first write records `mirrors`, the
 * copies the user sees as mirrors today, since no earlier record says otherwise. Refused while a Deploy runs the last one.
 */
export const stagePlacement = Effect.fn("VolumePlacement.stage")(function* (
  organizationId: string,
  input: { readonly environment: EnvironmentRef; readonly volumeId: string; readonly staged: Placement | null; readonly mirrors: readonly MachineId[] },
) {
  const current = yield* readRow(organizationId, input.volumeId);
  if (yield* deploying(organizationId, current)) return { ok: false, refusal: DEPLOYING } satisfies PlacementWrite;
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
    setWhere: eq(volumePlacement.organizationId, organizationId),
  }).returning();
  if (row === undefined) return { ok: false, refusal: { code: "conflict", message: "This Volume belongs to another organization.", details: null } } satisfies PlacementWrite;
  return { ok: true, placement: view(row) } satisfies PlacementWrite;
});

/** Drop what the Volume has staged. Refused while a Deploy runs it. */
export const discardPlacement = Effect.fn("VolumePlacement.discard")(function* (organizationId: string, volumeId: string) {
  const current = yield* readRow(organizationId, volumeId);
  if (current === undefined) return { ok: true, placement: null } as const;
  if (yield* deploying(organizationId, current)) return { ok: false, refusal: DEPLOYING } as const;
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle.update(volumePlacement)
    .set({ staged: null, deploymentId: null, updatedAt: new Date() })
    .where(eq(volumePlacement.volumeId, current.volumeId))
    .returning();
  return { ok: true, placement: row === undefined ? null : view(row) } as const;
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
