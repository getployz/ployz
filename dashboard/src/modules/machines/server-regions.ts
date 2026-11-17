import type { Region } from "@ployz/sdk";

/** Each Server's `region` label once: what the Store's region queries hear the Servers hold. */
export function regionLabels(machines: readonly { readonly region: string | null }[]): string[] {
  return [...new Set(machines.flatMap(({ region }) => region === null ? [] : [region]))].sort();
}

/** The region a Server's label puts it in. Labels compare without case, as placement does. */
export function regionOf(label: string | null, regions: readonly Region[]): Region | undefined {
  const key = label?.toLowerCase();
  return regions.find((region) => region.key === key);
}

/**
 * What a Server's region shows as: its region's name, or the label itself when it is no region's. Null when it has no
 * label, or while the names haven't arrived.
 */
export function regionName(label: string | null, regions: readonly Region[] | undefined): string | null {
  if (label === null || regions === undefined) return null;
  return regionOf(label, regions)?.name ?? label;
}
