import { createFileRoute } from "@tanstack/react-router";
import { ReadMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { readMachineEnrollment } from "#/modules/machines/enrollment.server";
import { handleOrganizationCliRequest } from "#/routes/api/cli/-handlers";
import { cliRoute } from "#/server/api-route";

/** `ployz server add --wait`: whether a Server has joined through an enrollment token. */
export const Route = createFileRoute("/api/cli/servers/enrollment")({
  server: {
    handlers: cliRoute({ POST: ({ request }) => handleOrganizationCliRequest(request, ReadMachineEnrollmentInput, readMachineEnrollment) }),
  },
});
