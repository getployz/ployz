import { queryOptions, useQuery } from "@tanstack/react-query";
import type { EnvironmentRef } from "@ployz/sdk";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { listOffsiteCopiesServerFn, readVolumeOffsiteServerFn } from "./volume-offsite.functions";

export const volumeOffsiteQueryOptions = (organizationSlug: string, environment: EnvironmentRef, volumeId: string, served: boolean) => queryOptions({
  queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "offsite"] as const,
  queryFn: () => readVolumeOffsiteServerFn({ data: { organizationSlug, environment, volumeId } }),
  enabled: served,
  staleTime: 60_000,
});

/** Asks only where the writer serves offsite, since each read lists the bucket from the writer. */
export function useVolumeOffsiteRead(organizationSlug: string, environment: EnvironmentRef, volumeId: string, served: boolean) {
  return useQuery(volumeOffsiteQueryOptions(organizationSlug, environment, volumeId, served)).data ?? null;
}

export const offsiteCopiesQueryOptions = (organizationSlug: string, environment: EnvironmentRef) => queryOptions({
  queryKey: [...volumeRunKeys.all, organizationSlug, "offsite", environment.project, environment.environment] as const,
  queryFn: () => listOffsiteCopiesServerFn({ data: { organizationSlug, environment } }),
  staleTime: 60_000,
});

/** The Environment's offsite copies, for the canvas cards. */
export function useOffsiteCopies(organizationSlug: string, environment: EnvironmentRef) {
  return useQuery(offsiteCopiesQueryOptions(organizationSlug, environment)).data ?? [];
}
