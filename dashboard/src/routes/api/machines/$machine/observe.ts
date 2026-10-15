import { createFileRoute } from "@tanstack/react-router";
import { handleObservePush } from "#/routes/api/machines/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/machines/$machine/observe")({
  server: {
    handlers: {
      POST: async ({ request, params }) =>
        runAppEffect(handleObservePush(request, params.machine), { signal: request.signal }).catch(publicErrorResponse),
    },
  },
});
