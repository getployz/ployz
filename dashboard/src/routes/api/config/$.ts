import { createFileRoute } from "@tanstack/react-router";
import { handleConfigRequest } from "#/routes/api/config/-config.handler";
import { apiRoute } from "#/server/api-route";

/** `POST /api/config/read` and `/api/config/write`: the `ployz` CLI's way into the Config Store. */
export const Route = createFileRoute("/api/config/$")({
  server: {
    handlers: apiRoute({ POST: ({ request }) => handleConfigRequest(request) }, { clientAddress: true }),
  },
});
