import { createFileRoute } from "@tanstack/react-router";
import { handleAgentRequest } from "#/routes/api/cli/agent/-agent.handler";
import { cliRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/cli/agent/ask")({
  server: {
    handlers: cliRoute({ POST: ({ request }) => handleAgentRequest(request, "ask") }),
  },
});
