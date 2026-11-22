import { createFileRoute } from "@tanstack/react-router";
import { Schema } from "effect";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { handleOrgChangesRequest } from "#/routes/api/org/-changes.handler";
import { organizationSearchRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/org/changes")({
  validateSearch: Schema.toStandardSchemaV1(Schema.Struct({ organizationSlug: organizationSlugSchema })),
  server: {
    handlers: organizationSearchRoute(handleOrgChangesRequest),
  },
});
