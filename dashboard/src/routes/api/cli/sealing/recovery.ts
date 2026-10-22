import { createFileRoute } from "@tanstack/react-router";
import { CliReadSealingRecoveryInput } from "#/modules/machines/sealing-key";
import { readCliSealingRecovery } from "#/modules/machines/sealing-key.server";
import { serveCli } from "#/routes/api/cli/-handlers";
import { handleSealingRequest } from "#/routes/api/cli/sealing/-sealing.handler";
import { methodNotAllowed } from "#/server/public-error";

export const Route = createFileRoute("/api/cli/sealing/recovery")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleSealingRequest(request, CliReadSealingRecoveryInput, readCliSealingRecovery)),
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
