import { createFileRoute } from "@tanstack/react-router";
import { ReadMachineEnrollmentInput } from "#/modules/machines/enrollment";
import { readMachineEnrollment } from "#/modules/machines/enrollment.server";
import { handleCliRequest, serveCli } from "#/routes/api/cli/-handlers";

/** `ployz server add --wait`: whether a Server has joined through an enrollment token. */
export const Route = createFileRoute("/api/cli/servers/enrollment")({
  server: {
    handlers: {
      POST: async ({ request }) =>
        serveCli(request, handleCliRequest(request, ReadMachineEnrollmentInput, readMachineEnrollment)),
    },
  },
});
