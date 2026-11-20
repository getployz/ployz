import { isbotMatch } from "isbot";

export const isBotUserAgent = (ua: string) => isbotMatch(ua) !== null;

const CACHE_LIMIT = 50_000;
const verdicts = new Map<string, boolean>();

/** One agent string is classified once per process: the same few thousand agents arrive in every window. The cache is bounded and starts over when full. */
export function botsAmong<Ua extends { readonly ua: string }>(uas: ReadonlyArray<Ua>): Ua[] {
  return uas.filter(({ ua }) => {
    const known = verdicts.get(ua);
    if (known !== undefined) return known;
    if (verdicts.size >= CACHE_LIMIT) verdicts.clear();
    const verdict = isBotUserAgent(ua);
    verdicts.set(ua, verdict);
    return verdict;
  });
}
