import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { handleRuntimeEventsRequest } from "#/routes/api/runtime/-events.handler";
import { organizationSearchRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/runtime/events")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ organizationSlug: organizationSlugSchema })),
  server: {
    handlers: organizationSearchRoute(handleRuntimeEventsRequest),
  },
});
