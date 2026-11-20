import { createFileRoute } from "@tanstack/react-router";
import { handleAnalyticsSalts } from "#/routes/api/machines/-handlers";
import { publicErrorResponse, methodNotAllowed } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/machines/$machine/analytics-salts")({
  server: {
    handlers: {
      POST: async ({ request, params }) =>
        runAppEffect(handleAnalyticsSalts(request, params.machine), { signal: request.signal }).catch(publicErrorResponse),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
