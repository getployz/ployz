import { createFileRoute } from "@tanstack/react-router";
import { handleAgentRequest } from "#/routes/api/cli/agent/-agent.handler";
import { serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

export const Route = createFileRoute("/api/cli/agent/act")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleAgentRequest(request, "act")),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
