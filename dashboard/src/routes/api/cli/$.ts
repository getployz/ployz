import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { handleCliRequest } from "#/routes/api/cli/-cli.handler";
import { serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

function handle(request: Request) {
  return serveCli(request, Effect.map(handleCliRequest(request), (body) => body instanceof Response ? body : Response.json(body)));
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
