import { createFileRoute } from "@tanstack/react-router";
import { getRequestIP } from "@tanstack/react-start/server";
import { Effect } from "effect";
import { observedIp } from "#/routes/api/checkin/-handler";
import { handleAgentSignup, signupMethodNotAllowed } from "#/routes/api/agent/-signup.handler";
import { AppConfig } from "#/server/config.server";
import { publicErrorResponse, onlyMethods } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

export const Route = createFileRoute("/api/agent/signup")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const peer = getRequestIP();
        return runAppEffect(
          Effect.gen(function* () {
            const { auth } = yield* AppConfig;
            return yield* handleAgentSignup(observedIp(request.headers, auth.clientIpHeaders, peer));
          }),
          { signal: request.signal },
        ).catch(publicErrorResponse);
      },
      GET: signupMethodNotAllowed,
      ANY: onlyMethods("POST", "GET"),
    },
  },
});
