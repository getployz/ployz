import { createFileRoute } from "@tanstack/react-router";
import { githubCallbackResponse } from "#/routes/api/github/-connect.handler";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/github/callback")({
  server: { handlers: apiRoute({ GET: ({ request }) => githubCallbackResponse(request) }) },
});
