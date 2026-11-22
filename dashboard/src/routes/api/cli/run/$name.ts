import { createFileRoute } from "@tanstack/react-router";
import { handleRunRequest } from "#/routes/api/cli/-handlers";
import { cliRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/cli/run/$name")({
  server: {
    handlers: cliRoute<{ name: string }>({ POST: ({ request, params }) => handleRunRequest(request, params.name) }),
  },
});
