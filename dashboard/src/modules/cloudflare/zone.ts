const LABEL = /^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$/u;

/** The zone as Cloudflare names it: lowercase, with no scheme, wildcard or trailing dot. */
export function normalizeZone(typed: string) {
  return typed.trim().toLowerCase().replace(/^https?:\/\//u, "").replace(/^\*\./u, "").replace(/\.$/u, "");
}

/**
 * Why `typed` can't be the zone Ployz puts a Cluster's generated names under, or null when it can. Generated names sit
 * one label below the zone, which the free Universal SSL certificate covers; a zone that is itself a subdomain of one
 * of the account's zones would put them two labels down, where it does not. `zones` is the account's zone names, or
 * null before they are listed.
 */
export function zoneProblem(typed: string, zones: readonly string[] | null) {
  const zone = normalizeZone(typed);
  if (zone === "") return "Pick one of your Cloudflare zones.";
  const labels = zone.split(".");
  if (labels.length < 2 || !labels.every((label) => LABEL.test(label))) return "Use a domain name such as example.com.";
  if (zones === null || zones.includes(zone)) return null;
  const parent = zones.find((candidate) => zone.endsWith(`.${candidate}`));
  if (parent !== undefined) {
    return `${zone} is under your zone ${parent}. Use ${parent}. Addresses go one level below the zone.`;
  }
  return `${zone} isn't a zone in this Cloudflare account.`;
}

/** The record setup creates and no other: one wildcard CNAME to the tunnel. The zone's apex is never touched. */
export function wildcardRecord(zone: string, tunnelId: string) {
  return { type: "CNAME", name: `*.${zone}`, content: `${tunnelId}.cfargotunnel.com`, proxied: true, ttl: 1 } as const;
}

export type CloudflareZone = { readonly id: string; readonly name: string; readonly accountId: string };

/** The permission a refused Cloudflare call lacked, as the token's creation page names it. */
export const PERMISSIONS = {
  zones: "Zone: Zone: Read",
  dns: "Zone: DNS: Edit",
  tunnel: "Account: Cloudflare Tunnel: Edit",
} as const;
export type PermissionKey = keyof typeof PERMISSIONS;
