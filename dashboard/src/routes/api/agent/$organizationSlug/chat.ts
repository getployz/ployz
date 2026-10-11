import { createFileRoute } from "@tanstack/react-router";
import { handleAgentChat, handleAgentThread } from "#/routes/api/agent/$organizationSlug/-chat.handler";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** The agent sidebar's conversation in one Organization: `POST` runs a turn, `GET` hydrates a thread. */
export const Route = createFileRoute("/api/agent/$organizationSlug/chat")({
  server: {
    handlers: {
      POST: async ({ request, params }) =>
        runAppEffect(handleAgentChat(request, params.organizationSlug), { signal: request.signal }).catch(publicErrorResponse),
      GET: async ({ request, params }) =>
        runAppEffect(handleAgentThread(request, params.organizationSlug), { signal: request.signal }).catch(publicErrorResponse),
    },
  },
});
