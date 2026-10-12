import { useState } from "react";
import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { ConfigWritten, EnvironmentRef, MachineId, MachineName } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import { environmentKey, refetchAfterWrite } from "#/modules/config-store/store-view.queries";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { useActiveVolumeRuns, useWatchVolumeRun, volumeMembersQueryOptions } from "#/modules/volume-run/volume-run.queries";
import {
  appliedMirrors, observePlacement, type Placement, placementChanges, placementGroup, placementLoss, type PlacementLoss, type PlacementVolume, type RestorePoint,
  stagedOffServer, storageStep, undoPlacement,
} from "./volume-placement";
import { deleteCopyServerFn, discardPlacementServerFn, stagePlacementServerFn, stageRestoreServerFn } from "./volume-placement.functions";
import { useVolumePlacements, volumePlacementKeys } from "./volume-placement.queries";

type PlacementResult = { readonly ok: true } | { readonly ok: false; readonly refusal: { readonly message: string } };

/** Settle placement writes. One that touched the Store (a restore staged or discarded) refreshes `environment`'s views too. */
function useSettle(failure: string, organizationSlug: string, environment: EnvironmentRef) {
  const queryClient = useQueryClient();
  const [pending, setPending] = useState(false);
  const settle = (writes: Promise<PlacementResult>[]) => {
    setPending(true);
    Promise.all(writes).then((results) => {
      for (const result of results) if (!result.ok) toast.error(result.refusal.message);
      void queryClient.invalidateQueries({ queryKey: volumePlacementKeys.all });
      void refetchAfterWrite(queryClient, organizationSlug, environmentKey(environment));
    }, (error: Error) => toast.error(toErrorMessage(error, failure)))
      .finally(() => setPending(false));
  };
  return { settle, pending };
}

/** Stage or discard a Volume's placement. A refusal toasts; the row then arrives on the change stream. */
export function useStagePlacement(organizationSlug: string, environment: EnvironmentRef, volume: PlacementVolume) {
  const { settle, pending } = useSettle(`Could not change ${volume.name}`, organizationSlug, environment);
  const stage = (staged: Placement | null, mirrors: readonly MachineId[]) =>
    settle([stagePlacementServerFn({ data: { organizationSlug, volumeId: volume.id, environment, staged, mirrors: [...mirrors] } })]);
  const discard = () => settle([discardPlacementServerFn({ data: { organizationSlug, volumeId: volume.id, environment } })]);
  return { stage, discard, pending };
}

/** Stage a new Volume restored from one of `volume`'s restore points; it takes over `volume`'s mounts at the next Deploy. */
export function useStageRestore(organizationSlug: string, environment: EnvironmentRef, volume: PlacementVolume) {
  const { settle, pending } = useSettle(`Could not restore ${volume.name}`, organizationSlug, environment);
  const restore = (point: RestorePoint) => settle([stageRestoreServerFn({ data: {
    organizationSlug, volumeId: volume.id, environment, guid: point.guid, name: point.name, at: Math.floor(point.at.getTime() / 1000),
  } })]);
  return { restore, pending };
}

/** Delete an old copy now, and say how it ended once the Server finishes. */
export function useDeleteCopy(organizationSlug: string, environment: EnvironmentRef, volume: PlacementVolume) {
  const queryClient = useQueryClient();
  const { watch } = useWatchVolumeRun(organizationSlug, (run) => {
    if (run?.state === "done") toast.success("Copy deleted");
    else toast.error(run?.message ?? `Could not delete the copy of ${volume.name}`);
    void queryClient.invalidateQueries({ queryKey: volumePlacementKeys.all });
  });
  return (server: MachineName) => {
    void deleteCopyServerFn({ data: { organizationSlug, volumeId: volume.id, environment, server } }).then((result) => {
      if (result.ok) watch(result.run.id);
      else toast.error(result.refusal.message);
    }, (error: Error) => { toast.error(toErrorMessage(error, `Could not delete the copy of ${volume.name}`)); });
  };
}

export type StagedPlacements = {
  readonly groups: readonly ChangeGroup[];
  readonly loss: PlacementLoss;
  readonly count: number;
  readonly discard: (volumeId: string) => void;
  readonly undo: (volumeId: string, path: string) => void;
  /** Discards every staged row after `discardStore`, which takes the Store's half, including each staged restore's Volume. */
  readonly discardAll: (discardStore: () => Promise<ConfigWritten | undefined>) => void;
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
  const failure = "Could not discard the Volume changes";
  const { settle } = useSettle(failure, organizationSlug, environment);
  const staged = rows.flatMap((row, index) => {
    const volume = volumes.find((entry) => entry.id === row.volumeId);
    const answered = members[index]?.data;
    if (volume === undefined || answered === undefined) return [];
    const changes = placementChanges(volume, answered, row);
    return changes.length === 0 ? [] : [{ volume, row, members: answered, changes }];
  });
  const now = new Date();
  const groups = staged.map(({ volume, row, changes }) => placementGroup(volume, changes, now, row.unreached));
  const discardOf = (volumeId: string) => discardPlacementServerFn({ data: { organizationSlug, volumeId, environment } });
  return {
    groups,
    loss: placementLoss(staged.flatMap(({ changes }) => changes)),
    count: groups.reduce((total, group) => total + group.changeCount, 0),
    discard: (volumeId) => settle([discardOf(volumeId)]),
    undo: (volumeId, path) => {
      const entry = staged.find(({ volume }) => volume.id === volumeId);
      const change = entry?.changes[Number(path)];
      if (entry === undefined || change === undefined) return;
      if (change.kind === "seed") return settle([discardOf(volumeId)]);
      const mirrors = appliedMirrors(observePlacement(entry.members), entry.row);
      settle([stagePlacementServerFn({ data: {
        organizationSlug, volumeId, environment, staged: undoPlacement(entry.volume, entry.members, entry.row, change), mirrors: [...mirrors],
      } })]);
    },
    // Every staged row, not just the groups shown: a restore whose Volume the Store discard takes may not have answered yet.
    discardAll: (discardStore) => {
      discardStore().then(() => settle(rows.map((row) => discardOf(row.volumeId))), () => undefined);
    },
  };
}

export function usePlacementStaged(organizationSlug: string, environment: EnvironmentRef, volume: PlacementVolume) {
  const row = useVolumePlacements(organizationSlug).data?.find((entry) => entry.volumeId === volume.id) ?? null;
  const waiting = row?.staged != null && row.deploymentId === null;
  const members = useQuery({ ...volumeMembersQueryOptions(organizationSlug, environment, volume.id), enabled: waiting }).data;
  return waiting && members !== undefined && placementChanges(volume, members, row).length > 0;
}

/** Each running Deploy's storage step, for the bottom bar and the Deployment page. */
export function useStorageSteps(organizationSlug: string) {
  const rows = useVolumePlacements(organizationSlug).data ?? [];
  const runs = useActiveVolumeRuns(organizationSlug).data ?? [];
  return (deploymentId: string) => storageStep(deploymentId, rows, runs);
}

/** Whether the Environment holds copy changes no Deploy has taken yet. */
export function useCopiesWait(organizationSlug: string, environmentId: string) {
  return (useVolumePlacements(organizationSlug).data ?? [])
    .some((row) => row.environmentId === environmentId && row.staged !== null && row.deploymentId === null);
}

/** How many Volume primaries on `server` are staged to move off it. */
export function useStagedOffServer(organizationSlug: string, server: string) {
  const rows = useVolumePlacements(organizationSlug).data ?? [];
  return stagedOffServer(server, rows, useRuntimeLens(organizationSlug).volumeCopies);
}

