import { createFileRoute } from "@tanstack/react-router";
import { CliRotateSealingRecoveryInput } from "#/modules/machines/sealing-key";
import { rotateCliSealingRecovery } from "#/modules/machines/sealing-key.server";
import { handleCliRequest, serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

export const Route = createFileRoute("/api/cli/sealing/rotate")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleCliRequest(request, CliRotateSealingRecoveryInput, rotateCliSealingRecovery)),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
