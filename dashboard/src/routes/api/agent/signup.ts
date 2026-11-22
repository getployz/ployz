import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { handleAgentSignup, signupMethodNotAllowed } from "#/routes/api/agent/-signup.handler";
import { apiRoute, clientIp } from "#/server/api-route";

export const Route = createFileRoute("/api/agent/signup")({
  server: {
    handlers: apiRoute(
      { POST: () => Effect.flatMap(clientIp, handleAgentSignup) },
      { clientAddress: true, refuseOthers: signupMethodNotAllowed },
    ),
  },
});
