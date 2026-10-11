import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { AppConfig } from "#/server/config.server";
import { runAppEffect } from "#/server/run.server";

/**
 * The marketing site (MARKETING_ORIGIN) owns every page the app doesn't: /, /home, /blog, robots.txt.
 * Hosted Cloud proxies those paths to it so both live on one origin. The hook is route server handlers
 * (a `/$` splat plus `/` and `/home`), not a Nitro or global request middleware: static files and every
 * app route already win by route ranking, the handler runs before SSR, and `next()` falls back to the
 * app's own 404 page without rendering it first just to throw it away.
 *
 * Visitor credentials never cross: Cookie and Authorization stay here and Set-Cookie never comes back.
 */

const FORWARDED_REQUEST_HEADERS = ["accept", "accept-language", "user-agent", "x-forwarded-for"];
// fetch decodes the body, so content-encoding and content-length would describe bytes we no longer send. Link points
// agents at the Markdown copy and llms.txt; X-Robots-Tag keeps those copies out of search results.
const PASSED_RESPONSE_HEADERS = ["content-type", "cache-control", "etag", "last-modified", "vary", "link", "x-robots-tag"];
const TIMEOUT = "5 seconds";

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

export function marketingRequestHeaders(request: Request): Headers {
  const url = new URL(request.url);
  const headers = new Headers();
  for (const name of FORWARDED_REQUEST_HEADERS) {
    const value = request.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  headers.set("x-forwarded-host", request.headers.get("x-forwarded-host") ?? url.host);
  headers.set("x-forwarded-proto", request.headers.get("x-forwarded-proto") ?? url.protocol.slice(0, -1));
  return headers;
}

/** The marketing site's answer to send the visitor, or undefined for the app's 404 (a 404 there too). */
export function marketingResponse(upstream: Response, marketingOrigin: URL): Response | undefined {
  if (upstream.status === 404) return undefined;
  const headers = new Headers();
  for (const name of PASSED_RESPONSE_HEADERS) {
    const value = upstream.headers.get(name);
    if (value !== null) headers.set(name, value);
  }
  const location = upstream.headers.get("location");
  if (location !== null) {
    const target = new URL(location, upstream.url || marketingOrigin);
    // A redirect within the marketing site stays on the visitor's origin.
    headers.set(
      "location",
      target.origin === marketingOrigin.origin ? `${target.pathname}${target.search}${target.hash}` : location,
    );
  }
  return new Response(upstream.body, { status: upstream.status, statusText: upstream.statusText, headers });
}

export const fetchMarketing = Effect.fn("Marketing.fetch")(function* (
  request: Request,
  marketingOrigin: URL,
  fetchImpl: Fetch = fetch,
) {
  const method = request.method.toUpperCase();
  if (method !== "GET" && method !== "HEAD") return undefined;
  const { pathname, search } = new URL(request.url);
  return yield* Effect.tryPromise((signal) =>
    fetchImpl(`${marketingOrigin.origin}${pathname}${search}`, {
      method,
      headers: marketingRequestHeaders(request),
      redirect: "manual",
      signal,
    }),
  ).pipe(
    Effect.timeout(TIMEOUT),
    Effect.map((upstream) => marketingResponse(upstream, marketingOrigin)),
    // ponytail: an unreachable marketing site degrades to the app's 404 rather than a 502.
    Effect.orElseSucceed(() => undefined),
  );
});

/** Proxy to the marketing site when MARKETING_ORIGIN is set; undefined means let the app answer. */
export function proxyMarketing(request: Request): Promise<Response | undefined> {
  return runAppEffect(
    Effect.gen(function* () {
      const { marketingOrigin } = yield* AppConfig;
      if (marketingOrigin === undefined) return undefined;
      return yield* fetchMarketing(request, marketingOrigin);
    }),
    { signal: request.signal },
  );
}
