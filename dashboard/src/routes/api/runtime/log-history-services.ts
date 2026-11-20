import { createFileRoute } from "@tanstack/react-router";
import { Option, Schema } from "effect";
import { logSearchSchema } from "#/modules/runtime/container-logs.server";
import { readHistoryServices } from "#/modules/runtime/log-history-page.server";
import { runAppEffect } from "#/server/run.server";
import { methodNotAllowed, publicErrorResponse, Validation } from "#/server/public-error";

export const Route = createFileRoute("/api/runtime/log-history-services")({
  server: { handlers: {
    GET: async ({ request }) => {
      try {
        const search = Schema.decodeUnknownOption(logSearchSchema)(Object.fromEntries(new URL(request.url).searchParams));
        if (Option.isNone(search)) return publicErrorResponse(new Validation({ message: "Invalid log selection." }));
        const result = await runAppEffect(readHistoryServices(request, search.value), { signal: request.signal });
        return Response.json(result, { headers: { "Cache-Control": "private, no-store" } });
      } catch (cause) { return publicErrorResponse(cause); }
    },
    ANY: (): Response => methodNotAllowed(Route),
  } },
});
