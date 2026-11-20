import { createFileRoute } from "@tanstack/react-router";
import { handleMachineOtlp } from "#/routes/api/machines/-handlers";
import { publicErrorResponse, methodNotAllowed } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/machines/otlp/v1/metrics")({
  server: {
    handlers: {
      POST: async ({ request }) => runAppEffect(handleMachineOtlp(request, "metrics"), { signal: request.signal }).catch(publicErrorResponse),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
