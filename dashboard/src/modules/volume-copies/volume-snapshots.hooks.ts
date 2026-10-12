import { useState } from "react";
import { queryOptions, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { EnvironmentRef } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import { volumePlacementKeys } from "#/modules/volume-placement/volume-placement.queries";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import type { Snapshot } from "./volume-copies";
import { listVolumeSnapshotsServerFn, setVolumeSnapshotServerFn } from "./volume-snapshots.functions";
import { type SnapshotEdit, snapshotFromWire } from "./volume-snapshots";

export function volumeSnapshotsQueryOptions(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  return queryOptions({
    queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "snapshots"] as const,
    queryFn: () => listVolumeSnapshotsServerFn({ data: { organizationSlug, environment, volumeId } }),
    staleTime: 15_000,
    refetchInterval: 60_000,
  });
}

/** The writer's snapshots, newest first, and the epoch an edit must name; null until listed or when the list is refused. */
export type SnapshotList = { readonly epoch: number | null; readonly snapshots: readonly Snapshot[] };

const done = (edit: SnapshotEdit) => {
  switch (edit.kind) {
    case "take":
      return "Snapshot taken";
    case "keep":
      return edit.kept ? "Snapshot kept" : "Not kept";
    case "delete":
      return "Snapshot deleted";
  }
};

export function useVolumeSnapshots(organizationSlug: string, environment: EnvironmentRef, volumeId: string, served: boolean) {
  const queryClient = useQueryClient();
  const options = volumeSnapshotsQueryOptions(organizationSlug, environment, volumeId);
  const listed = useQuery({ ...options, enabled: served }).data;
  const [pending, setPending] = useState(false);
  const list: SnapshotList | null = served && listed?.ok === true
    ? { epoch: listed.snapshots.epoch, snapshots: listed.snapshots.snapshots.map(snapshotFromWire).sort((a, b) => b.at.getTime() - a.at.getTime()) }
    : null;

  const edit = (change: SnapshotEdit) => {
    if (list?.epoch == null) return;
    setPending(true);
    setVolumeSnapshotServerFn({ data: { organizationSlug, environment, volumeId, expect: list.epoch, edit: change } }).then((result) => {
      if (!result.ok) {
        toast.error(result.refusal.message);
        void queryClient.invalidateQueries({ queryKey: options.queryKey });
        return;
      }
      queryClient.setQueryData(options.queryKey, result);
      if (change.kind === "delete") void queryClient.invalidateQueries({ queryKey: volumePlacementKeys.all });
      toast.success(done(change));
    }, (error: Error) => toast.error(toErrorMessage(error, "Could not change the snapshot")))
      .finally(() => setPending(false));
  };
  return {
    list,
    pending,
    take: () => edit({ kind: "take" }),
    keep: (id: string, kept: boolean) => edit({ kind: "keep", id, kept }),
    remove: (id: string) => edit({ kind: "delete", id }),
  };
}
