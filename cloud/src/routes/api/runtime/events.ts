import { createFileRoute } from "@tanstack/react-router";
import { Option, Schema } from "effect";
import { organizationSlugSchema } from "#/modules/organization/tables";
import type { RuntimeWatch } from "#/modules/runtime/runtime-events.server";
import {
  openRuntimeWatch,
  type OpenedRuntimeWatch,
} from "#/modules/runtime/runtime-watch.server";
import {
  handleRuntimeEventsRequest,
  runtimeEventsValidationError,
} from "#/routes/api/runtime/-events.handler";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const RuntimeEventsSearch = Schema.Struct({
  organizationSlug: organizationSlugSchema,
});
const runtimeEventsSearchSchema = Schema.toStandardSchemaV1(RuntimeEventsSearch, {
  parseOptions: { onExcessProperty: "error" },
});
const decodeRuntimeEventsSearch = Schema.decodeUnknownOption(RuntimeEventsSearch);

function toHttpRuntimeWatch(watch: OpenedRuntimeWatch): RuntimeWatch {
  if (watch.status !== "connected") return watch;
  return {
    status: "connected",
    frames: watch.frames,
    close: () => runAppEffect(watch.close),
  };
}

const openRuntimeWatchRequest = async (
  input: Parameters<typeof openRuntimeWatch>[0],
) => toHttpRuntimeWatch(await runAppEffect(openRuntimeWatch(input)));

export const Route = createFileRoute("/api/runtime/events")({
  validateSearch: runtimeEventsSearchSchema,
  server: {
    handlers: {
      GET: async ({ request }) => {
        const searchResult = decodeRuntimeEventsSearch(
          Object.fromEntries(new URL(request.url).searchParams),
          { onExcessProperty: "error" },
        );
        if (Option.isNone(searchResult)) {
          return publicErrorResponse(runtimeEventsValidationError());
        }
        return handleRuntimeEventsRequest(
          request,
          searchResult.value.organizationSlug,
          { openRuntimeWatch: openRuntimeWatchRequest },
        );
      },
    },
  },
});
