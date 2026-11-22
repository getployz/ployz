import { createFileRoute } from "@tanstack/react-router";
import { serveRegistryProof } from "#/modules/agent/remote-mcp.server";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/.well-known/mcp-registry-auth")({
  server: {
    handlers: apiRoute({ ANY: ({ request }) => serveRegistryProof(request) }),
  },
});
