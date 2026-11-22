import { createFileRoute } from "@tanstack/react-router";
import { handleMachineOtlp } from "#/routes/api/machines/-handlers";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/machines/otlp/v1/metrics")({
  server: {
    handlers: apiRoute({ POST: ({ request }) => handleMachineOtlp(request, "metrics") }),
  },
});
