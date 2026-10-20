import { createFileRoute } from "@tanstack/react-router";
import { serveMcpMetadata } from "#/modules/agent/remote-mcp.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const handle = ({ request }: { request: Request }) =>
  runAppEffect(serveMcpMetadata(request), { signal: request.signal }).catch(publicErrorResponse);

// Clients that don't find the /mcp-suffixed document ask the root, so it serves the same metadata.
export const Route = createFileRoute("/.well-known/oauth-protected-resource/")({
  server: { handlers: { ANY: handle } },
});
