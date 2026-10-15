import { createFileRoute } from "@tanstack/react-router";
import { handleGithubCallback } from "#/routes/api/github/-connect.handler";

export const Route = createFileRoute("/api/github/callback")({
  server: { handlers: { GET: async ({ request }) => handleGithubCallback(request) } },
});
