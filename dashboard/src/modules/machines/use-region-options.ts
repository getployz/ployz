import { regionLabels, regionOptions, type RegionOption } from "#/modules/machines/server-regions";
import { useRegions } from "#/modules/machines/server-regions.hooks";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";

/**
 * The regions a choice offers: those the Servers are in, with the names they were given, and each of `kept` even when no
 * Server is in it. Only any region until the Servers have been read.
 */
export function useRegionOptions(organizationSlug: string, kept: readonly string[] = []): RegionOption[] {
  const { machines, status } = useRuntimeLens(organizationSlug);
  // Read once the Servers are known: a read with no labels yet is asked again under another key when they arrive.
  const names = useRegions(organizationSlug, status === "connecting" ? undefined : regionLabels(machines));
  return regionOptions(machines, names, kept);
}
