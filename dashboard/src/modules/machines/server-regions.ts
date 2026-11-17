/** Regions compare without case, as placement does. */
export function knownRegions(machines: readonly { readonly region: string | null }[]): string[] {
  const byLowercase = new Map<string, string>();
  for (const { region } of machines) {
    if (region !== null && !byLowercase.has(region.toLowerCase())) byLowercase.set(region.toLowerCase(), region);
  }
  return [...byLowercase.values()].sort((left, right) => left.localeCompare(right));
}
