import { createFileRoute } from "@tanstack/react-router";
import { handleMachineSetupReport } from "#/routes/api/enroll/-handlers";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/enroll/report")({
  server: {
    handlers: apiRoute({ POST: ({ request }) => handleMachineSetupReport(request) }),
  },
});
