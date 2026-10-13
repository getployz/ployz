import type { EnvironmentRef } from "@ployz/sdk";
import { queryOptions } from "@tanstack/react-query";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { listVolumeSnapshotsServerFn, readSnapshotPolicyServerFn } from "./volume-snapshots.functions";

export function volumeSnapshotsQueryOptions(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  return queryOptions({
    queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "snapshots"] as const,
    queryFn: () => listVolumeSnapshotsServerFn({ data: { organizationSlug, environment, volumeId } }),
    staleTime: 15_000,
    refetchInterval: 60_000,
  });
}

export function snapshotPolicyQueryOptions(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  return queryOptions({
    queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "snapshot-policy"] as const,
    queryFn: () => readSnapshotPolicyServerFn({ data: { organizationSlug, environment, volumeId } }),
    staleTime: 60_000,
  });
}
