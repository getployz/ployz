import { queryOptions, useQuery } from "@tanstack/react-query";
import type { EnvironmentRef } from "@ployz/sdk";
import { volumeRunKeys } from "#/modules/volume-run/volume-run.queries";
import { listOffsiteCopiesServerFn, readVolumeOffsiteServerFn } from "./volume-offsite.functions";

export const volumeOffsiteQueryOptions = (organizationSlug: string, environment: EnvironmentRef, volumeId: string) => queryOptions({
  queryKey: [...volumeRunKeys.all, organizationSlug, volumeId, "offsite"] as const,
  queryFn: () => readVolumeOffsiteServerFn({ data: { organizationSlug, environment, volumeId } }),
  staleTime: 60_000,
});

export function useVolumeOffsiteRead(organizationSlug: string, environment: EnvironmentRef, volumeId: string) {
  return useQuery(volumeOffsiteQueryOptions(organizationSlug, environment, volumeId)).data ?? null;
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
