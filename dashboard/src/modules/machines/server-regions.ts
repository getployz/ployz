import type { NamedRegion } from "@ployz/sdk";

export const DEFAULT_REGION = "default";

/**
 * Each distinct Server `region` label, plus the default region, which a Server can join before any Server is in it.
 */
export function regionLabels(machines: readonly { readonly region: string | null }[]): string[] {
  return [...new Set([DEFAULT_REGION, ...machines.flatMap(({ region }) => region === null ? [] : [region])])].sort();
}

/**
 * The region a new Environment runs in: that of the first Server by name that has one, in lowercase as placement reads
 * it. Null, any region, while no Server has one.
 */
export function firstServerRegion(machines: readonly { readonly name: string; readonly region: string | null }[]): string | null {
  const first = machines.filter(({ region }) => region !== null).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0))[0];
  return first?.region?.toLowerCase() ?? null;
}

/** Labels compare without case, as placement does. */
export function regionOf(label: string | null, regions: readonly NamedRegion[]): NamedRegion | undefined {
  const key = label?.toLowerCase();
  return regions.find((region) => region.key === key);
}

/**
 * The region's name, or the label itself when it is no region's. Null with no label or before the names arrive.
 */
export function regionName(label: string | null, regions: readonly NamedRegion[] | undefined): string | null {
  if (label === null || regions === undefined) return null;
  return regionOf(label, regions)?.name ?? label;
}
