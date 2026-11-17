import { createFileRoute } from "@tanstack/react-router";
import { handleAgentCallReport } from "#/routes/api/cli/agent/-calls.handler";
import { serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

export const Route = createFileRoute("/api/cli/agent/calls")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleAgentCallReport(request)),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
