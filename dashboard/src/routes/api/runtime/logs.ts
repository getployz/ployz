import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { logSearchSchema, openContainerLogs } from "#/modules/runtime/container-logs.server";
import { containerLogResponse, hiddenLogResponse, offlineLogResponse, whileLogsShown } from "#/modules/runtime/container-log-events.server";
import { logHistorySchema, readHistoryPage } from "#/modules/runtime/log-history-page.server";
import { apiRoute, decodeJsonBody, decodeSearch } from "#/server/api-route";
import { Validation } from "#/server/public-error";
import type { AppServices } from "#/server/runtime.server";

const invalidSelection = () => new Validation({ message: "Invalid log selection." });

export const Route = createFileRoute("/api/runtime/logs")({
  server: {
    handlers: apiRoute({
      GET: ({ request }) =>
        Effect.gen(function* () {
          const search = yield* decodeSearch(logSearchSchema, request, invalidSelection());
          const result = yield* openContainerLogs(request, search);
          if (result.type === "offline") return offlineLogResponse(request);
          if (result.type === "hidden") return hiddenLogResponse(request);
          const run = Effect.runPromiseWith(yield* Effect.context<AppServices>());
          const { logsShown } = result;
          const events = logsShown ? whileLogsShown(result.events, () => run(logsShown)) : result.events;
          return containerLogResponse(request, events, () => run(result.close));
        }),
      POST: ({ request }) =>
        Effect.gen(function* () {
          const search = yield* decodeJsonBody(logHistorySchema, request, invalidSelection());
          const page = yield* readHistoryPage(request, search);
          return Response.json(page, { headers: { "Cache-Control": "private, no-store" } });
        }),
    }),
  },
});
