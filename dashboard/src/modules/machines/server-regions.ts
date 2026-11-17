import type { Region } from "@ployz/sdk";

export const DEFAULT_REGION = "default";

/**
 * Each distinct Server `region` label, plus the default region, which a Server can join before any Server is in it.
 */
export function regionLabels(machines: readonly { readonly region: string | null }[]): string[] {
  return [...new Set([DEFAULT_REGION, ...machines.flatMap(({ region }) => region === null ? [] : [region])])].sort();
}

/** Labels compare without case, as placement does. */
export function regionOf(label: string | null, regions: readonly Region[]): Region | undefined {
  const key = label?.toLowerCase();
  return regions.find((region) => region.key === key);
}

/**
 * The region's name, or the label itself when it is no region's. Null with no label or before the names arrive.
 */
export function regionName(label: string | null, regions: readonly Region[] | undefined): string | null {
  if (label === null || regions === undefined) return null;
  return regionOf(label, regions)?.name ?? label;
}
