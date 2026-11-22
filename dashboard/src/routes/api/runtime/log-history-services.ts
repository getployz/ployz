import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { logSearchSchema } from "#/modules/runtime/container-logs.server";
import { readHistoryServices } from "#/modules/runtime/log-history-page.server";
import { apiRoute, decodeSearch } from "#/server/api-route";
import { Validation } from "#/server/public-error";

export const Route = createFileRoute("/api/runtime/log-history-services")({
  server: {
    handlers: apiRoute({
      GET: ({ request }) =>
        Effect.gen(function* () {
          const search = yield* decodeSearch(logSearchSchema, request, new Validation({ message: "Invalid log selection." }));
          const services = yield* readHistoryServices(request, search);
          return Response.json(services, { headers: { "Cache-Control": "private, no-store" } });
        }),
    }),
  },
});
