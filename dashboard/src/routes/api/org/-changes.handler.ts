import { Effect } from "effect";
import type { ChangeName } from "#/collections/read.contract";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";
import { isClientDisconnect, publicErrorResponse } from "#/server/public-error";
import { runRedactedLog } from "#/server/log-redaction";

export type OrgChangesHandlerDeps = {
  /** Refuses non-members. */
  authorize: (organizationSlug: string) => Promise<{ organizationId: string }>;
  /** Where every stream starts: the current horizon. */
  currentCursor: () => Promise<string>;
  /** Collections changed since `since`, and the cursor to read from next; null once the Organization is gone. */
  readChanges: (input: { organizationId: string; since: string }) => Promise<{ cursor: string; collections: ChangeName[] } | null>;
};

const POLL_MS = 250;

/**
 * Organization change stream: it first names the Organization the slug resolved to, then each event names the
 * collections to refetch. It never resumes: every connect starts at the current horizon, and the client's refetch on
 * `open` covers the gap. It ends when the Organization is gone, so the client reconnects and learns what the slug names now.
 */
export async function handleOrgChangesRequest(request: Request, organizationSlug: string, deps: OrgChangesHandlerDeps) {
  let organizationId: string;
  try {
    ({ organizationId } = await deps.authorize(organizationSlug));
  } catch (cause) {
    return publicErrorResponse(cause);
  }
  // Taken before the response opens: the client refetches on `open`, so every change at or after
  // this cursor is either in that refetch or announced by the stream.
  const cursor = await deps.currentCursor().catch(logReadFailure);
  // EventSource never reconnects after a non-200 response, so a failed read still opens the stream,
  // sends `retry:`, and ends at once: the client reconnects and tries again.
  return eventStreamResponse(request.signal,
    (signal) => cursor === undefined ? noEvents() : orgChangeEvents(organizationId, cursor, deps, signal),
    { heartbeatMs: 15_000, retryMs: 1000 });
}

function logReadFailure(cause: unknown) {
  // A client that disconnected mid-read is routine.
  const log = isClientDisconnect(cause) ? Effect.logDebug : Effect.logError;
  runRedactedLog(log("Organization change log read failed.", cause));
  return undefined;
}

async function* noEvents(): AsyncGenerator<string> {}

// On the global setTimeout rather than node:timers/promises, which fake timers can't reach.
function sleep(ms: number, signal: AbortSignal) {
  return new Promise<void>((resolve) => {
    const wake = () => {
      clearTimeout(timer);
      signal.removeEventListener("abort", wake);
      resolve();
    };
    const timer = setTimeout(wake, ms);
    signal.addEventListener("abort", wake, { once: true });
  });
}

// ponytail: one poll loop per open stream; move to one loop per instance if open tabs reach the thousands.
async function* orgChangeEvents(organizationId: string, start: string, deps: OrgChangesHandlerDeps, signal: AbortSignal) {
  let cursor = start;
  yield sseEvent({ event: "organization", data: { id: organizationId } });
  while (!signal.aborted) {
    const changes = await deps.readChanges({ organizationId, since: cursor }).catch(logReadFailure);
    // Ending the stream on a failed read or a removed Organization makes EventSource reconnect, and its `open` refetch catches up.
    if (!changes) return;
    if (changes.collections.length > 0) yield sseEvent({ event: "changes", data: { collections: changes.collections } });
    cursor = changes.cursor;
    await sleep(POLL_MS, signal);
  }
}
