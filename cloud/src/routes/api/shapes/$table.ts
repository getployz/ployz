import { createFileRoute } from "@tanstack/react-router";
import { handleTableSyncRequest } from "#/electric/table-sync-request.server";
import { runAppEffect } from "#/server/run.server";
import { publicErrorResponse } from "#/server/public-error";

function handleRequest(request: Request, table: string) {
  return runAppEffect(
    handleTableSyncRequest(request, table),
    { signal: request.signal },
  ).catch((cause) =>
    publicErrorResponse(cause, {
      headers: { "cache-control": "private, no-store" },
    }),
  );
}

export const Route = createFileRoute("/api/shapes/$table")({
  server: {
    handlers: {
      GET: ({ request, params }) => handleRequest(request, params.table),
      POST: ({ request, params }) => handleRequest(request, params.table),
    },
  },
});
