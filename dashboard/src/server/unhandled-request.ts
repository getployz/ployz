import { createMiddleware } from "@tanstack/react-start";
import { isNotFound } from "@tanstack/react-router";
import { Effect } from "effect";
import { preferredRepresentation } from "#/server/content-negotiation";
import { runRedactedLog } from "#/server/log-redaction";
import { encodePublicError, isClientDisconnect, statusForPublicError } from "#/server/public-error";

/** Whether a throw belongs to TanStack Start: a redirect or another Response, a notFound, or the client leaving. */
function startOwns(request: Request, cause: unknown) {
  return cause instanceof Response || isNotFound(cause) || request.signal.aborted || isClientDisconnect(cause);
}

/** A page that needs nothing that may be down: no database, no script, no asset. */
function errorPage(requestId: string) {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Something went wrong</title>
<style>
body { margin: 0; font: 15px/1.5 system-ui, sans-serif; color: #18181b; background: #fff; }
main { max-width: 32rem; margin: 15vh auto 0; padding: 0 1rem; }
h1 { font-size: 1.25rem; margin: 0 0 0.5rem; }
p { margin: 0 0 0.75rem; color: #52525b; }
code { font: 13px ui-monospace, monospace; color: #18181b; }
@media (prefers-color-scheme: dark) { body { color: #fafafa; background: #09090b; } p { color: #a1a1aa; } code { color: #fafafa; } }
</style>
</head>
<body>
<main>
<h1>Something went wrong</h1>
<p>Ployz Cloud could not answer this request. Try again in a moment.</p>
<p>If it keeps happening, tell us this request id: <code>${requestId}</code></p>
</main>
</body>
</html>
`;
}

/**
 * What a request gets when its handler throws something nothing handled, in place of h3's bare
 * `{"status":500,"unhandled":true,"message":"HTTPError"}`. A page gets a static page, and anything else the Public
 * Error JSON every API route answers, both with a request id. One log line carries the same id, the method, the path
 * and the cause. The query string stays out of the log, since a link may carry a token.
 */
export function unhandledRequestResponse(
  request: Request,
  cause: unknown,
  page: boolean,
  requestId: string = crypto.randomUUID(),
): Response {
  const { pathname } = new URL(request.url);
  runRedactedLog(Effect.logError("Unhandled request error.", cause).pipe(
    Effect.annotateLogs({ requestId, method: request.method, path: pathname }),
  ));
  const error = encodePublicError(cause);
  const status = statusForPublicError(error);
  const headers = { "cache-control": "no-store", "ployz-request-id": requestId };
  if (page && !pathname.startsWith("/api/") && preferredRepresentation(request.headers.get("accept")) === "html") {
    return new Response(errorPage(requestId), { status, headers: { ...headers, "content-type": "text/html; charset=utf-8" } });
  }
  return Response.json({ ...error, requestId }, { status, headers });
}

/** The outermost request middleware, so a throw from any later middleware or route handler lands here. */
export const unhandledRequestMiddleware = createMiddleware().server(async ({ request, next, handlerType }) => {
  try {
    return await next();
  } catch (cause) {
    if (startOwns(request, cause)) throw cause;
    return unhandledRequestResponse(request, cause, handlerType === "router");
  }
});
