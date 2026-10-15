import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { Auth } from "#/server/auth.server";
import { Forbidden } from "#/server/public-error";

/**
 * Refuses a request that a page on another origin made the browser send. The browser attaches the session cookie to
 * those on its own, so a route the cookie authenticates must come from the dashboard's own pages: Sec-Fetch-Site says
 * so, or, from a browser that doesn't send it, the Origin is one sign-in trusts.
 */
export const requireSameOrigin = Effect.fn("requireSameOrigin")(function* (request: Request) {
  const site = request.headers.get("sec-fetch-site");
  const origin = request.headers.get("origin");
  const sameOrigin = site !== null ? site === "same-origin" : origin !== null && (yield* (yield* Auth).isTrustedOrigin(origin));
  if (!sameOrigin) return yield* new Forbidden({ message: "Refused a cross-site request." });
});
