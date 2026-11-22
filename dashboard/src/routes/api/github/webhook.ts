import { createFileRoute } from "@tanstack/react-router";
import { handleGithubWebhookRequest } from "#/routes/api/github/-webhook.handler";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/github/webhook")({
  server: {
    handlers: apiRoute({ POST: ({ request }) => handleGithubWebhookRequest(request) }),
  },
});
