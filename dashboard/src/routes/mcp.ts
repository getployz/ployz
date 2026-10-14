import { createFileRoute } from "@tanstack/react-router";
import { serveRemoteMcp } from "#/modules/agent/remote-mcp.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const handle = ({ request }: { request: Request }) =>
  runAppEffect(serveRemoteMcp(request), { signal: request.signal }).catch(publicErrorResponse);

export const Route = createFileRoute("/mcp")({
  server: { handlers: { GET: handle, POST: handle, DELETE: handle, OPTIONS: handle } },
});
