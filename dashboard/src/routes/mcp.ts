import { createFileRoute } from "@tanstack/react-router";
import { serveRemoteMcp } from "#/modules/agent/remote-mcp.server";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/mcp")({
  server: {
    handlers: apiRoute({ ANY: ({ request }) => serveRemoteMcp(request) }),
  },
});
