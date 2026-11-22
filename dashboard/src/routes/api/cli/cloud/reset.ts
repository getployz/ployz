import { createFileRoute } from "@tanstack/react-router";
import { CliResetPendingEnrollmentInput } from "#/modules/machines/enrollment";
import { resetPendingOrganizationEnrollment } from "#/modules/machines/enrollment.server";
import { handleOrganizationCliRequest } from "#/routes/api/cli/-handlers";
import { cliRoute } from "#/server/api-route";

/** `ployz cloud reset`: give up a founding that never finished. */
export const Route = createFileRoute("/api/cli/cloud/reset")({
  server: {
    handlers: cliRoute({ POST: ({ request }) => handleOrganizationCliRequest(request, CliResetPendingEnrollmentInput, resetPendingOrganizationEnrollment) }),
  },
});
