import { createFileRoute } from "@tanstack/react-router";
import { CliRotateSealingRecoveryInput } from "#/modules/sealing/sealing-key";
import { rotateCliSealingRecovery } from "#/modules/sealing/sealing-key.server";
import { handleCliRequest } from "#/routes/api/cli/-handlers";
import { cliRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/cli/sealing/rotate")({
  server: {
    handlers: cliRoute({ POST: ({ request }) => handleCliRequest(request, CliRotateSealingRecoveryInput, rotateCliSealingRecovery) }),
  },
});
