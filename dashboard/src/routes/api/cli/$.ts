import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { handleCliRequest } from "#/routes/api/cli/-cli.handler";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

function handle(request: Request) {
  return runAppEffect(Effect.map(handleCliRequest(request), (body) => Response.json(body)), {
    signal: request.signal,
  }).catch(publicErrorResponse);
}

export const Route = createFileRoute("/api/cli/$")({
  server: {
    handlers: {
      GET: async ({ request }) => handle(request),
      POST: async ({ request }) => handle(request),
      DELETE: async ({ request }) => handle(request),
    },
  },
});
