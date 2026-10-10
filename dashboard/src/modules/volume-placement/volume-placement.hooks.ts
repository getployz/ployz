import { useState } from "react";
import { useQueries, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { EnvironmentRef, MachineId } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { useActiveVolumeRuns, volumeMembersQueryOptions } from "#/modules/volume-run/volume-run.queries";
import { type Placement, placementChanges, placementGroup, type PlacementVolume, storageStep } from "./volume-placement";
import { discardPlacementServerFn, stagePlacementServerFn } from "./volume-placement.functions";
import { useVolumePlacements, volumePlacementKeys } from "./volume-placement.queries";

type PlacementResult = { readonly ok: true } | { readonly ok: false; readonly refusal: { readonly message: string } };

function useSettle(failure: string) {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState(false);
  const settle = (writes: Promise<PlacementResult>[]) => {
    setPending(true);
    Promise.all(writes).then((results) => {
      for (const result of results) if (!result.ok) toast.error(result.refusal.message);
      void queryClient.invalidateQueries({ queryKey: volumePlacementKeys.all });
    }, (error: Error) => toast.error(toErrorMessage(error, failure)))
      .finally(() => setPending(false));
  };
  return { settle, pending };
}

/** Stage or discard a Volume's placement. A refusal toasts; the row then arrives on the change stream. */
export function useStagePlacement(organizationSlug: string, environment: EnvironmentRef, volume: PlacementVolume) {
  const { settle, pending } = useSettle(`Could not change ${volume.name}`);
  const stage = (staged: Placement | null, mirrors: readonly MachineId[]) =>
    settle([stagePlacementServerFn({ data: { organizationSlug, volumeId: volume.id, environment, staged, mirrors: [...mirrors] } })]);
  const discard = () => settle([discardPlacementServerFn({ data: { organizationSlug, volumeId: volume.id } })]);
  return { stage, discard, pending };
}

export type StagedPlacements = {
  readonly groups: readonly ChangeGroup[];
  readonly count: number;
  readonly discard: (volumeId: string) => void;
  readonly discardAll: () => void;
};

/**
 * The Environment's staged Volume placements as Details groups, worded from what the Servers answer for each Volume.
 * A Volume whose staged placement already holds counts nothing, and one a running Deploy took counts as that Deploy's.
 */
export function useStagedPlacements(
  organizationSlug: string, environment: EnvironmentRef, environmentId: string, volumes: readonly PlacementVolume[],
): StagedPlacements {
  const rows = (useVolumePlacements(organizationSlug).data ?? [])
    .filter((row) => row.environmentId === environmentId && row.staged !== null && row.deploymentId === null);
  const members = useQueries({ queries: rows.map((row) => volumeMembersQueryOptions(organizationSlug, environment, row.volumeId)) });
  const { settle } = useSettle("Could not discard the Volume changes");
  const groups = rows.flatMap((row, index) => {
    const volume = volumes.find((entry) => entry.id === row.volumeId);
    const answered = members[index]?.data;
    if (volume === undefined || answered === undefined) return [];
    const changes = placementChanges(volume, answered, row);
    return changes.length === 0 ? [] : [placementGroup(volume, changes)];
  });
  const discardOf = (volumeId: string) => discardPlacementServerFn({ data: { organizationSlug, volumeId } });
  return {
    groups,
    count: groups.reduce((total, group) => total + group.changeCount, 0),
    discard: (volumeId) => settle([discardOf(volumeId)]),
    discardAll: () => settle(groups.map((group) => discardOf(group.nodeId))),
  };
}

/** Each running Deploy's storage step, for the bottom bar and the Deployment page. */
export function useStorageSteps(organizationSlug: string) {
  const rows = useVolumePlacements(organizationSlug).data ?? [];
  const runs = useActiveVolumeRuns(organizationSlug).data ?? [];
  return (deploymentId: string) => storageStep(deploymentId, rows, runs);
}
