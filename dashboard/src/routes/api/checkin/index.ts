import { createFileRoute } from "@tanstack/react-router";
import { getRequestIP } from "@tanstack/react-start/server";
import { Effect } from "effect";
import { handleCheckin, observedIp } from "#/routes/api/checkin/-handler";
import { AppConfig } from "#/server/config.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/checkin/")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const peer = getRequestIP();
        return runAppEffect(
          Effect.gen(function* () {
            const { auth } = yield* AppConfig;
            return yield* handleCheckin(request, observedIp(request.headers, auth.clientIpHeaders, peer));
          }),
          { signal: request.signal },
        ).catch(publicErrorResponse);
      },
    },
  },
});
