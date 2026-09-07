import { createFileRoute } from "@tanstack/react-router";
import { handleGithubWebhookRequest } from "#/routes/api/github/-webhook.handler";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/github/webhook")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(handleGithubWebhookRequest(request), {
          signal: request.signal,
        }).catch(publicErrorResponse),
    },
  },
});
