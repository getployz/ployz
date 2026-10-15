import { createFileRoute } from "@tanstack/react-router";
import { handleMachineEnrollmentCallback } from "#/routes/api/enroll/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/enroll/callback")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(
          handleMachineEnrollmentCallback(request),
          { signal: request.signal },
        ).catch(publicErrorResponse),
    },
  },
});
