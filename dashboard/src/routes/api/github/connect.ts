import { createFileRoute } from "@tanstack/react-router";
import { handleGithubConnect } from "#/routes/api/github/-connect.handler";
import { onlyMethods } from "#/server/public-error";

export const Route = createFileRoute("/api/github/connect")({
  server: { handlers: { GET: async ({ request }) => handleGithubConnect(request), ANY: onlyMethods("GET") } },
});
