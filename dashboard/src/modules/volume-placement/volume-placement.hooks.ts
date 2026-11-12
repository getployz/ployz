import { useState } from "react";
import { type QueryClient, useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { ConfigWritten, EnvironmentRef, MachineId, MachineName, VolumeListing } from "@ployz/sdk";
import { useMinute } from "#/hooks/use-minute";
import { toErrorMessage } from "#/lib/error-message";
import { environmentKey, invalidateViews } from "#/modules/config-store/store-view.queries";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { useActiveVolumeRuns, useWatchVolumeRun, volumeMembersQueryOptions } from "#/modules/volume-run/volume-run.queries";
import {
  appliedMirrors, observePlacement, type Placement, type PlacementChange, placementChanges, placementGroup, placementLoss, type PlacementLoss, type PlacementRow,
  type PlacementVolume, type RestorePoint,
  stagedOffServer, storageStep, undoPlacement,
} from "./volume-placement";
import { approveVolume, VolumeNotApproved, type VolumeScope } from "#/modules/approval-session/volume-approval";
import { readVolumeScope } from "#/modules/volume-run/volume-approval-scope";
import { type PlacementApproval, type PlacementAsk, seedAsk, stagedAsks } from "./placement-approvals";
import { deleteCopyServerFn, discardPlacementServerFn, keepPlacementApprovalsServerFn, stagePlacementServerFn, stageRestoreServerFn } from "./volume-placement.functions";
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
      void invalidateViews(queryClient, organizationSlug, new Set([environmentKey(environment)]));
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
  /**
   * Reads what each Volume workflow the next Deploy runs on a Server that enforces device approval names, then answers
   * with the prompts: they ask the person's passkey once for each workflow and keep the answers on the staged rows,
   * false when they cancel one. Both throw `VolumeNotApproved` or a refusal. The reads need no prompt, so a Deploy starts
   * them while it asks for its own approval.
   */
  readonly prepare: () => Promise<() => Promise<boolean>>;
};

/**
 * The Environment's staged Volume placements as Details groups, worded from what the Servers answer for each Volume.
 * A Volume whose staged placement already holds counts nothing, and one a running Deploy took counts as that Deploy's.
 */
export function useStagedPlacements(
  organizationSlug: string, environment: EnvironmentRef, environmentId: string, volumes: readonly (PlacementVolume & Pick<VolumeListing, "change">)[],
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
  const queryClient = useQueryClient();
  const now = useMinute() ?? new Date();
  const groups = staged.map(({ volume, row, changes }) => placementGroup(volume, changes, now, row.unreached));
  const discardOf = (volumeId: string) => discardPlacementServerFn({ data: { organizationSlug, volumeId, environment } });
  return {
    groups,
    // A Volume the Deploy deletes loses every copy; the Store confirms that itself, with its own details.
    loss: placementLoss(staged.filter(({ volume }) => volume.change !== "delete").flatMap(({ changes }) => changes)),
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
    prepare: async () => {
      // A Volume no Server enforces on asks nothing, so it reads nothing more. A Server that started enforcing since
      // refuses its run, and says to deploy again. A restore into a new Volume runs on its source's Servers.
      const asking = staged.filter(({ members: shown, changes }) =>
        shown.some((member) => member.answered && member.enforces === true) || changes.some((change) => change.kind === "seed"));
      const asked = await Promise.all(asking.map(async ({ volume, row, changes }) =>
        (await placementApprovalAsks(queryClient, organizationSlug, environment, volume, row, changes)).map((each) => ({ ...each, volumeId: volume.id }))));
      return async () => {
        const kept: (PlacementApproval & { readonly volumeId: string })[] = [];
        for (const { scope, ask, volumeId } of asked.flat()) {
          const approved = await approveVolume(scope, ask.change);
          if (approved === null) return false;
          if (approved === undefined) continue;
          kept.push({ volumeId, run: ask.run, machine: ask.machine, approval: approved.approval, ...(approved.service !== undefined && { service: approved.service }) });
        }
        if (kept.length === 0) return true;
        const result = await keepPlacementApprovalsServerFn({ data: { organizationSlug, environment, approvals: kept } });
        if (!result.ok) throw new VolumeNotApproved(result.refusal.message);
        return true;
      };
    },
    // Every staged row, not just the groups shown: a restore whose Volume the Store discard takes may not have answered yet.
    discardAll: (discardStore) => {
      // One after another: a restore's discard takes the Environment alone, so a sibling sent alongside finds it held.
      discardStore().then(() => settle(rows.reduce<Promise<PlacementResult>[]>((sent, row) => {
        const next = () => discardOf(row.volumeId);
        return [...sent, sent.at(-1)?.then(next, next) ?? next()];
      }, [])), () => undefined);
    },
  };
}

/**
 * The Volume workflows `volume`'s staged changes run, each drafted over its Servers' answers read afresh, with the
 * scope its approval names. A Move reads the spec its Service runs on the Server it leaves; a restore into a new Volume
 * is drafted on its source's Servers.
 */
async function placementApprovalAsks(
  queryClient: QueryClient, organizationSlug: string, environment: EnvironmentRef, volume: PlacementVolume, row: PlacementRow,
  changes: readonly PlacementChange[],
): Promise<{ readonly scope: VolumeScope; readonly ask: PlacementAsk }[]> {
  const seed = changes.find((change) => change.kind === "seed");
  if (seed !== undefined) {
    const source = await readVolumeScope(queryClient, organizationSlug, environment, seed.source.id);
    const primary = observePlacement(source.members).primary;
    if (primary === null) return [];
    const scope: VolumeScope = { ...source, dockerVolume: seed.dockerVolume, name: seed.volume.name };
    return [{ scope, ask: seedAsk(seed, source.dockerVolume, primary.id) }];
  }
  const moving = changes.find((change) => change.kind === "move" && change.how !== "restore");
  const from = moving?.kind === "move" ? moving.from?.id ?? null : null;
  const read = await readVolumeScope(queryClient, organizationSlug, environment, volume.id, from);
  return stagedAsks(volume, read.members, row, read).map((ask) => ({ scope: read, ask }));
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

