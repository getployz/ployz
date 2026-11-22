import { createFileRoute } from "@tanstack/react-router";
import { serveMcpMetadata } from "#/modules/agent/remote-mcp.server";
import { apiRoute } from "#/server/api-route";

// Clients that don't find the /mcp-suffixed document ask the root, so it serves the same metadata.
export const Route = createFileRoute("/.well-known/oauth-protected-resource/")({
  server: {
    handlers: apiRoute({ ANY: ({ request }) => serveMcpMetadata(request) }),
  },
});
