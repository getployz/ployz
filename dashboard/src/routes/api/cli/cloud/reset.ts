import { createFileRoute } from "@tanstack/react-router";
import { CliResetPendingEnrollmentInput } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollment } from "#/modules/machines/enrollment.server";
import { handleCliRequest, serveCli } from "#/routes/api/cli/-handlers";
import { onlyMethods } from "#/server/public-error";

/** `ployz cloud reset`: give up a founding that never finished. */
export const Route = createFileRoute("/api/cli/cloud/reset")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleCliRequest(request, CliResetPendingEnrollmentInput, resetPendingOrganizationEnrollment)),
      ANY: onlyMethods("POST"),
    },
  },
});
