import { createFileRoute } from "@tanstack/react-router";
import { handleWaitlistRequest } from "#/modules/waitlist/waitlist.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/waitlist")({
  server: {
    handlers: {
      POST: ({ request }) =>
        runAppEffect(handleWaitlistRequest(request), {
          signal: request.signal,
        }).catch(publicErrorResponse),
    },
  },
});
