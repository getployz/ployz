import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { collectionsOf } from "#/collections/change-sources";
import { readChangeWindow } from "#/modules/organization/change-log.server";

/** What an Organization's change stream names next: the collections changed since `since`, and the cursor after them. */
export const readOrganizationChanges = Effect.fn("OrgChanges.read")(function* (input: { organizationId: string; since: string }) {
  const window = yield* readChangeWindow(input);
  return { cursor: window.cursor, collections: collectionsOf(window.sourceTables) };
});
