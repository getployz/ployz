import { createFileRoute } from "@tanstack/react-router";
import { MintMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { mintCliMachineEnrollment } from "#/modules/machines/enrollment.server";
import { handleCliRequest } from "#/routes/api/cli/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** `ployz server add`: an enrollment token for the signed-in Organization. */
export const Route = createFileRoute("/api/cli/servers/enroll")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(
          handleCliRequest(request, MintMachineEnrollmentInput, mintCliMachineEnrollment),
          { signal: request.signal },
        ).catch(publicErrorResponse),
    },
  },
});
