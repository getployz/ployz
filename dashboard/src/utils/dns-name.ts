import { slugifySegment } from "#/utils/slug";

const DNS_LABEL_MAX = 63;

/**
 * What a typed Service, Volume or Config name is saved as. These names are DNS labels, so any input is converted;
 * `preview` is the converted name when it differs from what was typed, and `error` is set when nothing survives.
 */
export type DnsName = { readonly name: string; readonly preview: string | null; readonly error: string | null };

export function dnsName(raw: string): DnsName {
  const name = slugifySegment(raw).slice(0, DNS_LABEL_MAX).replace(/-+$/, "");
  if (!name) return { name, preview: null, error: raw.trim() ? "Use letters a to z or digits." : null };
  return { name, preview: name === raw ? null : name, error: null };
}
