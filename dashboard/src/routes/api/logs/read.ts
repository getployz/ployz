import { createFileRoute } from "@tanstack/react-router";
import { fromClient } from "#/routes/api/cli/-handlers";
import { handleLogsRead } from "#/routes/api/logs/-logs.handler";
import { methodNotAllowed, publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** `POST /api/logs/read`: the `ployz` CLI's way to an Environment's stored logs, removed Services' included. */
export const Route = createFileRoute("/api/logs/read")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(fromClient(request, handleLogsRead(request)), { signal: request.signal }).catch(publicErrorResponse),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
