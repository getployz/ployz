import type { EnvironmentRef } from "@ployz/sdk";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { VolumeCopies } from "#/modules/volume-copies/volume-copies";
import { useVolumeCopiesOf } from "#/modules/volume-copies/volume-copies.hooks";
import { mirroredVolumes, type PlacementVolume } from "#/modules/volume-placement/volume-placement";
import { useVolumePlacements } from "#/modules/volume-placement/volume-placement.queries";
import { useActiveVolumeRuns } from "#/modules/volume-run/volume-run.queries";

/** Each Volume's copies by Volume id, for the Volumes with a copy, a staged change or a run under way; the rest have none. */
export function useCanvasCopies(
  organizationSlug: string, environment: EnvironmentRef, environmentId: string, volumes: readonly PlacementVolume[],
): ReadonlyMap<string, VolumeCopies> {
  const rows = useVolumePlacements(organizationSlug).data ?? [];
  const runs = useActiveVolumeRuns(organizationSlug).data ?? [];
  const placed = mirroredVolumes(volumes.map((volume) => volume.id), rows, environmentId, useRuntimeLens(organizationSlug).volumeCopies);
  const running = volumes.flatMap((volume) => runs.some((run) => run.volume_id === volume.id) ? [volume.id] : []);
  return useVolumeCopiesOf(organizationSlug, environment, volumes, [...new Set([...placed, ...running])]);
}
