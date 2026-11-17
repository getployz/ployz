import type { Region } from "@ployz/sdk";

/** The region every Server joins unless it is put in another. */
export const DEFAULT_REGION = "default";

/**
 * What the Store's region queries hear the Servers hold: each Server's `region` label once, and the default region,
 * which a Server can join before any Server is in it.
 */
export function regionLabels(machines: readonly { readonly region: string | null }[]): string[] {
  return [...new Set([DEFAULT_REGION, ...machines.flatMap(({ region }) => region === null ? [] : [region])])].sort();
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
