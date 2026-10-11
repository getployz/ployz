import { createFileRoute } from "@tanstack/react-router";
import { handleAgentRequest } from "#/routes/api/cli/agent/-agent.handler";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/cli/agent/ask")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(handleAgentRequest(request, "ask"), { signal: request.signal }).catch(publicErrorResponse),
    },
  },
});
