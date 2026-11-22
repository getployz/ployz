import { Cause, Effect, Option, Stream } from "effect";
import { currentChangeCursor } from "#/collections/change-log/change-log.server";
import { readOrganizationChanges } from "#/collections/change-log/org-changes.server";
import { authorizeRequest } from "#/modules/organization/access.server";
import type { Database } from "#/server/database.server";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";
import { isClientDisconnect } from "#/server/public-error";

const POLL_INTERVAL = "250 millis";

const noChanges = (cause: Cause.Cause<unknown>) =>
  (isClientDisconnect(cause) ? Effect.logDebug : Effect.logError)("Organization change log read failed.", cause).pipe(Effect.as(null));

// ponytail: one poll loop per open stream; move to one loop per instance if open tabs reach the thousands.
const changeEvents = (organizationId: string, start: string) =>
  Stream.paginate({ since: start, first: true }, ({ since, first }) => Effect.gen(function* () {
    if (!first) yield* Effect.sleep(POLL_INTERVAL);
    const changes = yield* readOrganizationChanges({ organizationId, since }).pipe(Effect.catchCause(noChanges));
    // Ending the stream on a failed read or a removed Organization makes EventSource reconnect, and its `open` refetch catches up.
    if (changes === null) return [[], Option.none()] as const;
    const events = changes.collections.length > 0 ? [sseEvent({ event: "changes", data: { collections: changes.collections } })] : [];
    return [events, Option.some({ since: changes.cursor, first: false })] as const;
  }));

/**
 * Organization change stream: it first names the Organization the slug resolved to, then each event names the
 * collections to refetch. It never resumes: every connect starts at the current horizon, and the client's refetch on
 * `open` covers the gap. It ends when the Organization is gone, so the client reconnects and learns what the slug names now.
 */
export const handleOrgChangesRequest = Effect.fn("OrgChanges.stream")(function* (request: Request, organizationSlug: string) {
  const { organization } = yield* authorizeRequest(request.headers, organizationSlug, "org:read");
  // Taken before the response opens: the client refetches on `open`, so every change at or after
  // this cursor is either in that refetch or announced by the stream.
  const cursor = yield* currentChangeCursor().pipe(Effect.catchCause(noChanges));
  // The stream outlives this Effect, so it keeps this one's services.
  const services = yield* Effect.context<Database>();
  // EventSource never reconnects after a non-200 response, so a failed read still opens the stream,
  // sends `retry:`, and ends at once: the client reconnects and tries again.
  const events = cursor === null ? Stream.empty : Stream.make(sseEvent({ event: "organization", data: { id: organization.id } })).pipe(
    Stream.concat(changeEvents(organization.id, cursor)),
  );
  return eventStreamResponse(request.signal, () => Stream.toAsyncIterableWith(events, services), { heartbeatMs: 15_000, retryMs: 1000 });
});
