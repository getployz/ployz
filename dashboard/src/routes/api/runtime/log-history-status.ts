import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { historyStatusSchema, readHistoryStatus } from "#/modules/runtime/log-history-page.server";
import { apiRoute, decodeSearch } from "#/server/api-route";
import { Validation } from "#/server/public-error";

export const Route = createFileRoute("/api/runtime/log-history-status")({
  server: {
    handlers: apiRoute({
      GET: ({ request }) =>
        Effect.gen(function* () {
          const query = yield* decodeSearch(historyStatusSchema, request, new Validation({ message: "An organization is required." }));
          const status = yield* readHistoryStatus(request, query.organizationSlug);
          return Response.json(status, { headers: { "Cache-Control": "private, no-store" } });
        }),
    }),
  },
});
