import { createFileRoute } from "@tanstack/react-router";
import { Effect, Option, Schema } from "effect";
import { collectionsOf } from "#/collections/change-sources";
import { currentChangeCursor, readChangeWindow } from "#/modules/organization/change-log.server";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { authorizeRuntimeOrganization } from "#/modules/runtime/authorize-runtime-organization.server";
import { handleOrgChangesRequest } from "#/routes/api/org/-changes.handler";
import { publicErrorResponse, Validation } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

const OrgChangesSearch = Schema.Struct({ organizationSlug: organizationSlugSchema });
const decodeOrgChangesSearch = Schema.decodeUnknownOption(OrgChangesSearch);

export const Route = createFileRoute("/api/org/changes")({
  validateSearch: Schema.toStandardSchemaV1(OrgChangesSearch),
  server: {
    handlers: {
      GET: async ({ request }) => {
        const search = decodeOrgChangesSearch(Object.fromEntries(new URL(request.url).searchParams));
        if (Option.isNone(search)) return publicErrorResponse(new Validation({ message: "A valid organization slug is required" }));
        return handleOrgChangesRequest(request, search.value.organizationSlug, {
          authorize: (organizationSlug) =>
            runAppEffect(authorizeRuntimeOrganization({ headers: request.headers, organizationSlug }), { signal: request.signal }),
          currentCursor: () => runAppEffect(currentChangeCursor(), { signal: request.signal }),
          readChanges: (input) =>
            runAppEffect(readChangeWindow(input).pipe(
              Effect.map((window) => ({ cursor: window.cursor, collections: collectionsOf(window.sourceTables) })),
            ), { signal: request.signal }),
        });
      },
    },
  },
});
