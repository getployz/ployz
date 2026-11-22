import { createFileRoute } from "@tanstack/react-router";
import { handleMachineEnrollmentJoin } from "#/routes/api/enroll/-handlers";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/enroll/")({
  server: {
    handlers: apiRoute({ POST: ({ request }) => handleMachineEnrollmentJoin(request) }),
  },
});
