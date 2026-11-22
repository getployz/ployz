import { createFileRoute } from "@tanstack/react-router";
import { CliReadSealingRecoveryInput } from "#/modules/sealing/sealing-key";
import { readCliSealingRecovery } from "#/modules/sealing/sealing-key.server";
import { handleCliRequest } from "#/routes/api/cli/-handlers";
import { cliRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/cli/sealing/recovery")({
  server: {
    handlers: cliRoute({ POST: ({ request }) => handleCliRequest(request, CliReadSealingRecoveryInput, readCliSealingRecovery) }),
  },
});
