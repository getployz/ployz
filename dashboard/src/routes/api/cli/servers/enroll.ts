import { createFileRoute } from "@tanstack/react-router";
import { CliMintMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { mintCliMachineEnrollment } from "#/modules/machines/enrollment.server";
import { handleOrganizationCliRequest } from "#/routes/api/cli/-handlers";
import { cliRoute } from "#/server/api-route";

/** `ployz server add`: an enrollment token for the signed-in Organization. */
export const Route = createFileRoute("/api/cli/servers/enroll")({
  server: {
    handlers: cliRoute({ POST: ({ request }) => handleOrganizationCliRequest(request, CliMintMachineEnrollmentInput, mintCliMachineEnrollment) }),
  },
});
