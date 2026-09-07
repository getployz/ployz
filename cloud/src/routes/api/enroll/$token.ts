import { createFileRoute } from "@tanstack/react-router";
import { handleMachineEnrollmentJoin } from "#/routes/api/enroll/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/enroll/$token")({
  server: {
    handlers: {
      POST: async ({ request, params }) =>
        runAppEffect(
          handleMachineEnrollmentJoin(request, params.token),
          { signal: request.signal },
        ).catch(publicErrorResponse),
    },
  },
});
