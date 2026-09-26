import { projectContainerLog } from "#/modules/runtime/container-log.collection";
import { createFileRoute } from "@tanstack/react-router";
import { Option, Schema } from "effect";
import { logSearchSchema, openContainerLogs } from "#/modules/runtime/container-logs.server";
import { containerLogResponse, offlineLogResponse } from "#/modules/runtime/container-log-events.server";
import { runAppEffect } from "#/server/run.server";
import { publicErrorResponse, Validation } from "#/server/public-error";

export const Route = createFileRoute("/api/runtime/logs")({
  server: { handlers: { GET: async ({ request }) => {
    try {
      const search = Schema.decodeUnknownOption(logSearchSchema)(Object.fromEntries(new URL(request.url).searchParams));
      if (Option.isNone(search)) return publicErrorResponse(new Validation({ message: "Invalid log selection." }));
      const result = await runAppEffect(openContainerLogs(request, search.value), { signal: request.signal });
      if (result.type === "offline") return offlineLogResponse(request);
      return result.type === "history" ? Response.json({ ...result.page, records: result.page.records.map(projectContainerLog) }, { headers: { "Cache-Control": "private, no-store" } }) : containerLogResponse(request, result.events, () => runAppEffect(result.close));
    } catch (cause) { return publicErrorResponse(cause); }
  } } },
});
