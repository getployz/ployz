import { createFileRoute } from "@tanstack/react-router";
import { cliReply } from "#/routes/api/cli/-cli.handler";
import { serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

function handle(request: Request) {
  return serveCli(request, cliReply(request));
}

export const Route = createFileRoute("/api/cli/$")({
  server: {
    handlers: {
      GET: async ({ request }) => handle(request),
      POST: async ({ request }) => handle(request),
      DELETE: async ({ request }) => handle(request),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
