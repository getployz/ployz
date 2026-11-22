import { createFileRoute } from "@tanstack/react-router";
import { handleAgentCallReport } from "#/routes/api/cli/agent/-calls.handler";
import { cliRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/cli/agent/calls")({
  server: {
    handlers: cliRoute({ POST: ({ request }) => handleAgentCallReport(request) }),
  },
});
