import { createFileRoute } from "@tanstack/react-router";
import { Effect, Schema } from "effect";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { handleOrgChangesRequest } from "#/routes/api/org/-changes.handler";
import { apiRoute, decodeSearch } from "#/server/api-route";
import { Validation } from "#/server/public-error";

const OrgChangesSearch = Schema.Struct({ organizationSlug: organizationSlugSchema });

export const Route = createFileRoute("/api/org/changes")({
  validateSearch: Schema.toStandardSchemaV1(OrgChangesSearch),
  server: {
    handlers: apiRoute({
      GET: ({ request }) =>
        Effect.flatMap(
          decodeSearch(OrgChangesSearch, request, new Validation({ message: "A valid organization slug is required" })),
          ({ organizationSlug }) => handleOrgChangesRequest(request, organizationSlug),
        ),
    }),
  },
});
