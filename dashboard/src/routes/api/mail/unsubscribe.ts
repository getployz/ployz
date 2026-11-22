import { createFileRoute } from "@tanstack/react-router";
import { handleUnsubscribeRequest } from "#/routes/api/mail/-unsubscribe.handler";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/mail/unsubscribe")({
  server: {
    handlers: apiRoute({
      GET: ({ request }) => handleUnsubscribeRequest(request),
      POST: ({ request }) => handleUnsubscribeRequest(request),
    }),
  },
});
