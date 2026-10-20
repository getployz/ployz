import { createFileRoute } from "@tanstack/react-router";
import {
  handleLegacyEnrollment,
  handleMachineEnrollmentJoin,
} from "#/routes/api/enroll/-handlers";
import { methodNotAllowed, publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/enroll/")({
  server: {
    handlers: {
      GET: async ({ request }) => handleLegacyEnrollment(request),
      POST: async ({ request }) => {
        if (!request.headers.get("authorization")?.startsWith("Bearer ")) {
          return handleLegacyEnrollment(request);
        }
        return runAppEffect(
          handleMachineEnrollmentJoin(request),
          { signal: request.signal },
        ).catch(publicErrorResponse);
      },
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
