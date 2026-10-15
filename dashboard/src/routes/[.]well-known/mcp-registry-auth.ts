import { createFileRoute } from "@tanstack/react-router";
import { serveRegistryProof } from "#/modules/agent/remote-mcp.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const handle = ({ request }: { request: Request }) =>
  runAppEffect(serveRegistryProof(request), { signal: request.signal }).catch(publicErrorResponse);

export const Route = createFileRoute("/.well-known/mcp-registry-auth")({
  server: { handlers: { ANY: handle } },
});
