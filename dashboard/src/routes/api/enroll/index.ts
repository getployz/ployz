import { createFileRoute } from "@tanstack/react-router";
import { handleMachineEnrollmentJoin } from "#/routes/api/enroll/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/enroll/")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(
          handleMachineEnrollmentJoin(request),
          { signal: request.signal },
        ).catch(publicErrorResponse),
    },
  },
});
