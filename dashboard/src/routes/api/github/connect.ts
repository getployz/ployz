import { createFileRoute } from "@tanstack/react-router";
import { githubConnectResponse } from "#/routes/api/github/-connect.handler";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/github/connect")({
  server: { handlers: apiRoute({ GET: ({ request }) => githubConnectResponse(request) }) },
});
