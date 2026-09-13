import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { InngestClient } from "#/modules/inngest/client";
import { handleInngestHttp } from "#/modules/inngest/http";
import { AppConfig } from "#/server/config.server";
import { runAppEffect } from "#/server/run.server";
import { publicErrorResponse } from "#/server/public-error";

function handleInngestRequest(request: Request) {
  return runAppEffect(Effect.gen(function* () {
    const config = yield* AppConfig;
    const inngest = yield* InngestClient;
    return yield* handleInngestHttp(inngest, config.app.url.href, request);
  }).pipe(
    Effect.catchTag("InngestRequestError", (cause) =>
      Effect.succeed(publicErrorResponse(cause)),
    ),
  ), { signal: request.signal });
}

export const Route = createFileRoute("/api/inngest")({
  server: {
    handlers: {
      GET: ({ request }) => handleInngestRequest(request),
      POST: ({ request }) => handleInngestRequest(request),
      PUT: ({ request }) => handleInngestRequest(request),
    },
  },
});
