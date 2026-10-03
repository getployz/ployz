import { createFileRoute } from "@tanstack/react-router";
import { handleMachineSetupReport } from "#/routes/api/enroll/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/enroll/$token/report")({
  server: {
    handlers: {
      POST: async ({ request, params }) =>
        runAppEffect(
          handleMachineSetupReport(request, params.token),
          { signal: request.signal },
        ).catch(publicErrorResponse),
    },
  },
});
