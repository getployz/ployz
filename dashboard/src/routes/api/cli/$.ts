import { createFileRoute } from "@tanstack/react-router";
import { cliReply } from "#/routes/api/cli/-cli.handler";
import { cliRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/cli/$")({
  server: {
    handlers: cliRoute({
      GET: ({ request }) => cliReply(request),
      POST: ({ request }) => cliReply(request),
      DELETE: ({ request }) => cliReply(request),
    }),
  },
});
