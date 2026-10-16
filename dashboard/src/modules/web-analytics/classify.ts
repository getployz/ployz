import { isbotMatch } from "isbot";
import type { RequestKind } from "#/modules/web-analytics/tables";

export type RequestTraits = {
  readonly method: string;
  readonly status: number;
  readonly ua: string;
  readonly contentType: string | null;
  readonly secPurpose: string | null;
};

export type Classified = { readonly kind: RequestKind; readonly botReason: string | null };

const isSpeculative = (secPurpose: string | null) => secPurpose !== null && /prefetch|prerender/i.test(secPurpose);

export function classify(request: RequestTraits): Classified {
  const bot = isbotMatch(request.ua);
  if (bot !== null) return { kind: "bot", botReason: bot.toLowerCase() };
  const page = request.method === "GET"
    && request.status >= 200 && request.status < 300
    && request.contentType?.toLowerCase().startsWith("text/html") === true
    && !isSpeculative(request.secPurpose);
  return { kind: page ? "pageview" : "other", botReason: null };
}
