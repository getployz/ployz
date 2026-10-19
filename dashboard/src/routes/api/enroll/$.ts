import { createFileRoute } from "@tanstack/react-router";
import { handleLegacyEnrollment } from "#/routes/api/enroll/-handlers";

export const Route = createFileRoute("/api/enroll/$")({
  server: {
    handlers: {
      ANY: async ({ request }) => handleLegacyEnrollment(request),
    },
  },
});
