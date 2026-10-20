import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { collectionsOf } from "#/collections/change-sources";
import { OrganizationChangeLogFailure, readChangeWindow } from "#/modules/organization/change-log.server";
import { organization } from "#/modules/organization/tables";
import { Database } from "#/server/database.server";

/**
 * What an Organization's change stream names next: the collections changed since `since`, and the cursor after them.
 * Null once the Organization is gone, so the stream ends and its client reconnects to whatever the slug names now.
 */
export const readOrganizationChanges = Effect.fn("OrgChanges.read")(function* (input: { organizationId: string; since: string }) {
  const window = yield* readChangeWindow(input);
  const { drizzle } = yield* Database;
  const [live] = yield* drizzle.select({ id: organization.id }).from(organization).where(eq(organization.id, input.organizationId))
    .pipe(Effect.mapError((cause) => new OrganizationChangeLogFailure({ cause })));
  if (live === undefined) return null;
  return { cursor: window.cursor, collections: collectionsOf(window.sourceTables) };
});
