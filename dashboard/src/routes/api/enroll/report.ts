import { createFileRoute } from "@tanstack/react-router";
import { handleMachineSetupReport } from "#/routes/api/enroll/-handlers";
import { publicErrorResponse, methodNotAllowed } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/enroll/report")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(
          handleMachineSetupReport(request),
          { signal: request.signal },
        ).catch(publicErrorResponse),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
