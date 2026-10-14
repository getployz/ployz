import { createFileRoute } from "@tanstack/react-router";
import { serveMcpMetadata } from "#/modules/agent/remote-mcp.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const handle = ({ request }: { request: Request }) =>
  runAppEffect(serveMcpMetadata(request), { signal: request.signal }).catch(publicErrorResponse);

export const Route = createFileRoute("/.well-known/oauth-authorization-server/")({
  server: { handlers: { GET: handle, OPTIONS: handle } },
});
