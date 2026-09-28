import { projectContainerLog } from "./container-log.collection";
import type { LogEvent } from "@ployz/sdk";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";

// A stream that ends, however it ends, is retried by the browser after `retry`; the viewer never sees it drop.
const RETRY_MS = 3_000;
const OFFLINE_RETRY_MS = 5_000;

const TAIL_BUDGET_MS = 3_000;

type ContainerLogStreamEvent = LogEvent | { type: "live" };

/**
 * `live` follows the tail, so a viewer that sees it with no records knows the log is empty.
 * A tail slower than the budget stops holding everyone back: the rest of it is dropped and the follow begins,
 * so there `live` only means the budget ran out.
 * The follow repeats the tail: whatever was written between the two reads arrives, and the rows' ids drop the rest.
 */
export async function* backfillThenFollow(tail: AsyncIterable<LogEvent>, follow: AsyncIterable<LogEvent>): AsyncIterable<ContainerLogStreamEvent> {
  const reader = tail[Symbol.asyncIterator]();
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<"late">(resolve => { timer = setTimeout(resolve, TAIL_BUDGET_MS, "late"); });
  try {
    for (;;) {
      const next = await Promise.race([reader.next(), late]);
      if (next === "late" || next.done) break;
      yield next.value;
    }
  } finally {
    clearTimeout(timer);
    // A hung read queues this behind its pending `next()`; the request's abort is what releases it.
    void reader.return?.()?.catch(() => {});
  }
  yield { type: "live" };
  yield* follow;
}

/** Pull-driven delivery preserves every log record rather than coalescing watch snapshots. */
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
      if (event.type === "live") yield sseEvent({ event: "live", data: {} });
      else yield sseEvent({ event: "log", data: event.type === "record" ? { type: "record", record: projectContainerLog(event.record) } : event });
    }
  } catch {
    // The upstream broke; ending the stream makes the browser reconnect.
  }
}
