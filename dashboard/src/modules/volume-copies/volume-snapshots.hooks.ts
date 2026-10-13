import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { EnvironmentRef } from "@ployz/sdk";
import { toErrorMessage } from "#/lib/error-message";
import { volumePlacementKeys } from "#/modules/volume-placement/volume-placement.queries";
import type { Snapshot } from "./volume-copies";
import type { SnapshotPolicy } from "./snapshot-schedule";
import { saveSnapshotPolicyServerFn, setVolumeSnapshotServerFn } from "./volume-snapshots.functions";
import { snapshotPolicyQueryOptions, volumeSnapshotsQueryOptions } from "./volume-snapshots.queries";
import { type SnapshotEdit, snapshotFromWire } from "./volume-snapshots";

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

/** The Volume's snapshot policy; saving it prunes at once, so the snapshot list refreshes too. */
export function useSnapshotPolicy(organizationSlug: string, environment: EnvironmentRef, volumeId: string, served: boolean) {
  const queryClient = useQueryClient();
  const options = snapshotPolicyQueryOptions(organizationSlug, environment, volumeId);
  const policy = useQuery({ ...options, enabled: served }).data ?? null;
  const [pending, setPending] = useState(false);
  const save = (next: SnapshotPolicy) => {
    setPending(true);
    return saveSnapshotPolicyServerFn({ data: { organizationSlug, environment, volumeId, policy: next } }).then(({ deleted }) => {
      queryClient.setQueryData(options.queryKey, next);
      void queryClient.invalidateQueries({ queryKey: volumeSnapshotsQueryOptions(organizationSlug, environment, volumeId).queryKey });
      toast.success(deleted > 0 ? `Saved · ${deleted} deleted` : "Schedule saved");
      return true;
    }, (error: Error) => {
      toast.error(toErrorMessage(error, "Could not save the schedule"));
      return false;
    }).finally(() => setPending(false));
  };
  return { policy, pending, save };
}
