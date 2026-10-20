import { createFileRoute } from "@tanstack/react-router";
import { CliMintMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { mintCliMachineEnrollment } from "#/modules/machines/enrollment.server";
import { handleCliRequest, serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

/** `ployz server add`: an enrollment token for the signed-in Organization. */
export const Route = createFileRoute("/api/cli/servers/enroll")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleCliRequest(request, CliMintMachineEnrollmentInput, mintCliMachineEnrollment)),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
