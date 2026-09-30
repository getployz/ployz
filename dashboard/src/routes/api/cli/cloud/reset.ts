import { createFileRoute } from "@tanstack/react-router";
import { ResetPendingEnrollmentInput } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollment } from "#/modules/machines/enrollment.server";
import { handleCliRequest } from "#/routes/api/cli/-handlers";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** `ployz cloud reset`: give up a founding that never finished. */
export const Route = createFileRoute("/api/cli/cloud/reset")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        runAppEffect(
          handleCliRequest(request, ResetPendingEnrollmentInput, resetPendingOrganizationEnrollment),
          { signal: request.signal },
        ).catch(publicErrorResponse),
    },
  },
});
