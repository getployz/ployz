import { createFileRoute } from "@tanstack/react-router";
import { handleAnalyticsSalts } from "#/routes/api/machines/-handlers";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/machines/$machine/analytics-salts")({
  server: {
    handlers: apiRoute<{ machine: string }>({ POST: ({ request, params }) => handleAnalyticsSalts(request, params.machine) }),
  },
});
