import { createFileRoute } from "@tanstack/react-router";
import { CliRotateRecipientRecoveryInput } from "#/modules/machines/sealing-key";
import { rotateCliRecipientRecovery } from "#/modules/machines/sealing-key.server";
import { handleCliRequest, serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

export const Route = createFileRoute("/api/cli/sealing/recipient/rotate")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleCliRequest(request, CliRotateRecipientRecoveryInput, rotateCliRecipientRecovery)),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
