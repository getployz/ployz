import { createFileRoute } from "@tanstack/react-router";
import { handleRunRequest, serveCli } from "#/routes/api/cli/-handlers";
import { onlyMethods } from "#/server/public-error";

export const Route = createFileRoute("/api/cli/run/$name")({
  server: {
    handlers: {
      POST: async ({ request, params }) => serveCli(request, handleRunRequest(request, params.name)),
      ANY: onlyMethods("POST"),
    },
  },
});
