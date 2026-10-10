import { projectContainerLog, projectLogExit, projectLogGap } from "#/modules/runtime/container-log.collection";
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
      if (result.type === "stream") return containerLogResponse(request, result.events, () => runAppEffect(result.close));
      const { records, gaps, exits, failures, cursor } = result.page;
      return Response.json({ rows: [...records.map(projectContainerLog), ...gaps.map(projectLogGap), ...exits.map(projectLogExit)], failures, cursor }, { headers: { "Cache-Control": "private, no-store" } });
    } catch (cause) { return publicErrorResponse(cause); }
  } } },
});
