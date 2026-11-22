import { createFileRoute } from "@tanstack/react-router";
import { handleLogsRead } from "#/routes/api/logs/-logs.handler";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/logs/read")({
  server: {
    handlers: apiRoute({ POST: ({ request }) => handleLogsRead(request) }, { clientAddress: true }),
  },
});
