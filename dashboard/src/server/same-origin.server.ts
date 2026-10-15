import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { AppConfig } from "#/server/config.server";
import { Forbidden } from "#/server/public-error";

/**
 * Refuses a request that a page on another origin made the browser send. The browser attaches the session cookie to
 * those on its own, so a route the cookie authenticates must come from the dashboard's own pages: Sec-Fetch-Site says
 * so, or, from a browser that doesn't send it, the Origin is the dashboard's.
 */
export const requireSameOrigin = Effect.fn("requireSameOrigin")(function* (request: Request) {
  const config = yield* AppConfig;
  const site = request.headers.get("sec-fetch-site");
  const sameOrigin = site === null ? request.headers.get("origin") === config.app.url.origin : site === "same-origin";
  if (!sameOrigin) return yield* new Forbidden({ message: "Refused a cross-site request." });
});
