import { useQueries } from "@tanstack/react-query";
import type { EnvironmentRef } from "@ployz/sdk";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { VolumeCopies } from "#/modules/volume-copies/volume-copies";
import { copiesFromPlacement } from "#/modules/volume-copies/volume-copies.placement";
import { lastMirrorRefreshQueryOptions } from "#/modules/volume-copies/volume-copies.queries";
import { offsiteCopyOf, offsiteServed } from "#/modules/volume-copies/volume-offsite";
import { useOffsiteCopies } from "#/modules/volume-copies/volume-offsite.queries";
import { mirroredVolumes, observePlacement, type PlacementVolume } from "#/modules/volume-placement/volume-placement";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import { useActiveVolumeRuns, volumeMembersQueryOptions } from "#/modules/volume-run/volume-run.queries";

/** Each Volume's copies by Volume id, for the Volumes with a copy, a staged change or a run under way; the rest have none. */
export function useCanvasCopies(
  organizationSlug: string, environment: EnvironmentRef, environmentId: string, volumes: readonly PlacementVolume[],
): ReadonlyMap<string, VolumeCopies> {
  const lens = useRuntimeLens(organizationSlug);
  const rows = useVolumePlacements(organizationSlug).data ?? [];
  const runs = useActiveVolumeRuns(organizationSlug).data ?? [];
  const offsite = useOffsiteCopies(organizationSlug, environment);
  const placed = mirroredVolumes(volumes.map((volume) => volume.id), rows, environmentId, lens.volumeCopies);
  const running = volumes.flatMap((volume) => runs.some((run) => run.volume_id === volume.id) ? [volume.id] : []);
  const backedUp = volumes.flatMap((volume) => offsite.some((copy) => copy.volumeId === volume.id) ? [volume.id] : []);
  const ids = [...new Set([...placed, ...running, ...backedUp])];
  const members = useQueries({ queries: ids.map((id) => volumeMembersQueryOptions(organizationSlug, environment, id)) });
  const synced = useQueries({ queries: ids.map((id) => lastMirrorRefreshQueryOptions(organizationSlug, id)) });
  return new Map(ids.flatMap((id, index) => {
    const volume = volumes.find((entry) => entry.id === id);
    const answered = members[index]?.data;
    if (volume === undefined || answered === undefined) return [];
    const stored = offsite.find((copy) => copy.volumeId === id);
    const served = offsiteServed(lens.machines, observePlacement(answered).primary?.id ?? null);
    const copies = copiesFromPlacement({
      volume,
      members: answered,
      row: rows.find((row) => row.volumeId === id) ?? null,
      active: runs.find((run) => run.volume_id === id) ?? null,
      lastSync: synced[index]?.data?.ended ?? null,
      lastDone: synced[index]?.data?.done ?? null,
      offsite: stored !== undefined && served ? offsiteCopyOf(stored, null) : null,
    });
    return copies === null ? [] : [[id, copies] as const];
  }));
}
