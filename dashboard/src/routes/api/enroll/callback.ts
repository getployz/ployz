import { createFileRoute } from "@tanstack/react-router";
import { handleMachineEnrollmentCallback } from "#/routes/api/enroll/-handlers";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/enroll/callback")({
  server: {
    handlers: apiRoute({ POST: ({ request }) => handleMachineEnrollmentCallback(request) }),
  },
});
