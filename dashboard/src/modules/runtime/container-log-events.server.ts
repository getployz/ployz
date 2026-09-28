import { projectContainerLog } from "./container-log.collection";
import type { LogEvent } from "@ployz/sdk";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";

// A stream that ends, however it ends, is retried by the browser after `retry`; the viewer never sees it drop.
const RETRY_MS = 3_000;
const OFFLINE_RETRY_MS = 5_000;

export type ContainerLogStreamEvent = LogEvent | { type: "ready" };

/**
 * `ready` follows the whole tail, so a viewer that sees it with no records knows the log is empty.
 * The follow repeats the tail: whatever was written between the two reads arrives, and the rows' ids drop the rest.
 */
export async function* backfillThenFollow(tail: AsyncIterable<LogEvent>, follow: AsyncIterable<LogEvent>): AsyncIterable<ContainerLogStreamEvent> {
  yield* tail;
  yield { type: "ready" };
  yield* follow;
}

/** Pull-driven delivery preserves every log record rather than coalescing watch snapshots. `ready` becomes `live`. */
export function containerLogResponse(request: Request, events: AsyncIterable<ContainerLogStreamEvent>, close: () => Promise<void>) {
  return eventStreamResponse(request.signal, () => containerLogEvents(events), { heartbeatMs: 15_000, retryMs: RETRY_MS, onClose: close });
}

/** The organization's servers are unreachable: say so, then let the browser ask again. */
export function offlineLogResponse(request: Request) {
  return eventStreamResponse(request.signal, async function* () { yield sseEvent({ event: "offline", data: {} }); }, { heartbeatMs: 15_000, retryMs: OFFLINE_RETRY_MS });
}

async function* containerLogEvents(events: AsyncIterable<ContainerLogStreamEvent>) {
  try {
    for await (const event of events) {
      if (event.type === "ready") yield sseEvent({ event: "live", data: {} });
      else yield sseEvent({ event: "log", data: event.type === "record" ? { type: "record", record: projectContainerLog(event.record) } : event });
    }
  } catch {
    // The upstream broke; ending the stream makes the browser reconnect.
  }
}
