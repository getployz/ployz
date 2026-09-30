import { createFileRoute } from "@tanstack/react-router";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** `POST /api/config/read` and `/api/config/write`: the `ployz` CLI's way into the Config Store. */
export const Route = createFileRoute("/api/config/$")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(handleConfigRequest(request), { signal: request.signal }).catch(publicErrorResponse),
    },
  },
});
