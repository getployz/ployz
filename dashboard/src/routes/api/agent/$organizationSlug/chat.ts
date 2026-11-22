import { createFileRoute } from "@tanstack/react-router";
import { handleAgentChat, handleAgentThread } from "#/routes/api/agent/$organizationSlug/-chat.handler";
import { apiRoute } from "#/server/api-route";

/** The agent sidebar's conversation in one Organization: `POST` runs a turn, `GET` hydrates a thread. */
export const Route = createFileRoute("/api/agent/$organizationSlug/chat")({
  server: {
    handlers: apiRoute<{ organizationSlug: string }>({
      POST: ({ request, params }) => handleAgentChat(request, params.organizationSlug),
      GET: ({ request, params }) => handleAgentThread(request, params.organizationSlug),
    }),
  },
});
