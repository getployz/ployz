import { createFileRoute } from "@tanstack/react-router";
import { serveMcpMetadata } from "#/modules/agent/remote-mcp.server";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/.well-known/oauth-authorization-server/")({
  server: {
    handlers: apiRoute({ ANY: ({ request }) => serveMcpMetadata(request) }),
  },
});
