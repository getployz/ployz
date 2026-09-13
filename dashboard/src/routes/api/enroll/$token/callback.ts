import { createFileRoute } from "@tanstack/react-router";
import { handleMachineEnrollmentCallback } from "#/routes/api/enroll/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/enroll/$token/callback")({
  server: {
    handlers: {
      POST: async ({ request, params }) =>
        runAppEffect(
          handleMachineEnrollmentCallback(request, params.token),
          { signal: request.signal },
        ).catch(publicErrorResponse),
    },
  },
});
