import { createFileRoute } from "@tanstack/react-router";
import { Option, Schema } from "effect";
import { historyStatusSchema, readHistoryStatus } from "#/modules/runtime/log-history-page.server";
import { runAppEffect } from "#/server/run.server";
import { methodNotAllowed, publicErrorResponse, Validation } from "#/server/public-error";

export const Route = createFileRoute("/api/runtime/log-history-status")({
  server: { handlers: {
    GET: async ({ request }) => {
      try {
        const query = Schema.decodeUnknownOption(historyStatusSchema)(Object.fromEntries(new URL(request.url).searchParams));
        if (Option.isNone(query)) return publicErrorResponse(new Validation({ message: "An organization is required." }));
        const status = await runAppEffect(readHistoryStatus(request, query.value.organizationSlug), { signal: request.signal });
        return Response.json(status, { headers: { "Cache-Control": "private, no-store" } });
      } catch (cause) { return publicErrorResponse(cause); }
    },
    ANY: (): Response => methodNotAllowed(Route),
  } },
});
