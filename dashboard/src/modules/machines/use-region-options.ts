import { regionLabels, regionOptions, type RegionOption } from "#/modules/machines/server-regions";
import { useRegions } from "#/modules/machines/server-regions.hooks";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";

/**
 * The regions a choice offers: those the Servers are in, with the names they were given, and each of `kept` even when no
 * Server is in it. Only any region until the Servers have been read.
 */
export function useRegionOptions(organizationSlug: string, kept: readonly string[] = []): RegionOption[] {
  const { machines } = useRuntimeLens(organizationSlug);
  const names = useRegions(organizationSlug, regionLabels(machines));
  return regionOptions(machines, names, kept);
}
