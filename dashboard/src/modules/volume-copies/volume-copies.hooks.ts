import { useQueries } from "@tanstack/react-query";
import type { EnvironmentRef } from "@ployz/sdk";
import type { VolumeCopies } from "#/modules/volume-copies/volume-copies";
import { copiesFromPlacement } from "#/modules/volume-copies/volume-copies.placement";
import { mirrorStatusesQueryOptions } from "#/modules/volume-copies/mirror-sync/mirror-sync.queries";
import type { PlacementVolume } from "#/modules/volume-placement/volume-placement";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import { useActiveVolumeRuns, volumeMembersQueryOptions } from "#/modules/volume-run/volume-run.queries";

/** The copies of each Volume in `ids`, by Volume id, once its Servers have answered; a Volume without copies is left out. */
export function useVolumeCopiesOf(
  organizationSlug: string, environment: EnvironmentRef, volumes: readonly PlacementVolume[], ids: readonly string[],
): ReadonlyMap<string, VolumeCopies> {
  const rows = useVolumePlacements(organizationSlug).data ?? [];
  const runs = useActiveVolumeRuns(organizationSlug).data ?? [];
  const members = useQueries({ queries: ids.map((id) => volumeMembersQueryOptions(organizationSlug, environment, id)) });
  const synced = useQueries({ queries: ids.map((id) => mirrorStatusesQueryOptions(organizationSlug, id)) });
  return new Map(ids.flatMap((id, index) => {
    const volume = volumes.find((entry) => entry.id === id);
    const answered = members[index]?.data;
    if (volume === undefined || answered === undefined) return [];
    const copies = copiesFromPlacement({
      volume,
      members: answered,
      row: rows.find((row) => row.volumeId === id) ?? null,
      active: runs.find((run) => run.volume_id === id) ?? null,
      statuses: synced[index]?.data ?? [],
    });
    return copies === null ? [] : [[id, copies] as const];
  }));
}
