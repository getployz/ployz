import { createFileRoute } from "@tanstack/react-router";
import { CliReadRecipientRecoveryInput } from "#/modules/machines/sealing-key";
import { readCliRecipientRecovery } from "#/modules/machines/sealing-key.server";
import { handleCliRequest, serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

export const Route = createFileRoute("/api/cli/sealing/recipient")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleCliRequest(request, CliReadRecipientRecoveryInput, readCliRecipientRecovery)),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
