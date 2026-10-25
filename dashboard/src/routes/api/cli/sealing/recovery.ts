import { createFileRoute } from "@tanstack/react-router";
import { CliReadSealingRecoveryInput } from "#/modules/machines/sealing-key";
import { readCliSealingRecovery } from "#/modules/machines/sealing-key.server";
import { handleCliRequest, serveCli } from "#/routes/api/cli/-handlers";
import { methodNotAllowed } from "#/server/public-error";

export const Route = createFileRoute("/api/cli/sealing/recovery")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleCliRequest(request, CliReadSealingRecoveryInput, readCliSealingRecovery)),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
