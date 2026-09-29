import { createFileRoute } from "@tanstack/react-router";
import { ReadMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { readMachineEnrollment } from "#/modules/machines/enrollment.server";
import { handleCliRequest } from "#/routes/api/cli/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** `ployz server add --wait`: whether a Server has joined through an enrollment token. */
export const Route = createFileRoute("/api/cli/servers/enrollment")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(
          handleCliRequest(request, ReadMachineEnrollmentInput, readMachineEnrollment),
          { signal: request.signal },
        ).catch(publicErrorResponse),
    },
  },
});
