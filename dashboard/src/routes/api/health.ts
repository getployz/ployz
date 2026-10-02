import { createFileRoute } from "@tanstack/react-router";

// The platform's health check (Railway's healthcheckPath): 200 as soon as this server answers HTTP. It deliberately
// touches nothing else (no session, no database, no marketing proxy) so a deploy isn't failed by a dependency's blip.
export const Route = createFileRoute("/api/health")({
  server: {
    handlers: {
      GET: () => new Response("ok", { headers: { "content-type": "text/plain", "cache-control": "no-store" } }),
    },
  },
});
