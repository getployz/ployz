import { isRegion } from "#/modules/machines/server-policy";

/** Regions compare without case, as placement does. Of spellings that differ only in case, the valid one is kept. */
export function knownRegions(machines: readonly { readonly region: string | null }[]): string[] {
  const byLowercase = new Map<string, string>();
  for (const { region } of machines) {
    if (region === null) continue;
    const key = region.toLowerCase();
    const kept = byLowercase.get(key);
    if (kept === undefined || (!isRegion(kept) && isRegion(region))) byLowercase.set(key, region);
  }
  return [...byLowercase.values()].sort((left, right) => left.localeCompare(right));
}
