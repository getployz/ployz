import type { NamedRegion } from "@ployz/sdk";
import { firstServerRegion as firstRegion, parseRegionName } from "@ployz/sdk/config";
import { isRegionKey } from "#/modules/machines/server-policy";

export const DEFAULT_REGION = "default";

const NAME_RULE = "A region’s name is 1 to 40 characters on one line.";

/** Why `name` can't be a region's name, in the Engine's rule, or null when it can. */
export function regionNameProblem(name: string): string | null {
  try {
    parseRegionName(name);
    return null;
  } catch {
    return NAME_RULE;
  }
}

/**
 * Each distinct Server `region` label that is a region, plus the default region, which a Server can join before any
 * Server is in it. A label that is no region (`eu/west`, from an older Ployz) is left out, as the Engine leaves it.
 */
export function regionLabels(machines: readonly { readonly region: string | null }[]): string[] {
  return [...new Set([DEFAULT_REGION, ...machines.flatMap(({ region }) => region === null || !isRegionKey(region) ? [] : [region])])].sort();
}

/**
 * The region a new Environment runs in, by the Engine's one rule, which this hands every Server to: that of the
 * Cluster's first Server, in lowercase as placement reads it. Null, any region, when any Server has none, or when a
 * Server's subnet isn't known yet: a cluster that predates regions pins nothing.
 */
export function firstServerRegion(
  machines: readonly { readonly subnet: string | null; readonly joinedAt: number | null; readonly region: string | null }[],
): string | null {
  return firstRegion(machines.map(({ subnet, joinedAt, region }) => ({ subnet, joined: joinedAt, region })));
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

/** A region an Environment or a Service can run in: `key` null is any region, the one that names none. */
export type RegionOption = { readonly key: string | null; readonly name: string; readonly servers: readonly string[] };

export const ANY_REGION = "Any region";

/** Any region, then each region a Server is in with its Servers. A `kept` region no Server is in stays listed. */
export function regionOptions(
  machines: readonly { readonly name: string; readonly region: string | null }[],
  names: readonly NamedRegion[] | undefined,
  kept: readonly string[] = [],
): RegionOption[] {
  const servers = new Map<string, string[]>(kept.map((key) => [key, []]));
  for (const { name, region } of machines) {
    const key = region?.toLowerCase();
    if (key !== undefined && isRegionKey(key)) servers.set(key, [...(servers.get(key) ?? []), name]);
  }
  const regions = [...servers].map(([key, held]) => ({ key, name: regionName(key, names) ?? key, servers: held.sort() }));
  return [{ key: null, name: ANY_REGION, servers: [] }, ...regions.sort((a, b) => a.name.localeCompare(b.name))];
}
