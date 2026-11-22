import { createFileRoute } from "@tanstack/react-router";
import { Effect, Schema } from "effect";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { handleRuntimeEventsRequest } from "#/routes/api/runtime/-events.handler";
import { apiRoute, decodeSearch } from "#/server/api-route";
import { Validation } from "#/server/public-error";

const RuntimeEventsSearch = Schema.Struct({
  organizationSlug: organizationSlugSchema,
});
const runtimeEventsSearchSchema = Schema.toStandardSchemaV1(RuntimeEventsSearch);

export const Route = createFileRoute("/api/runtime/events")({
  validateSearch: runtimeEventsSearchSchema,
  server: {
    handlers: apiRoute({
      GET: ({ request }) =>
        Effect.flatMap(
          decodeSearch(RuntimeEventsSearch, request, new Validation({ message: "A valid organization slug is required" })),
          ({ organizationSlug }) => handleRuntimeEventsRequest(request, organizationSlug),
        ),
    }),
  },
});
