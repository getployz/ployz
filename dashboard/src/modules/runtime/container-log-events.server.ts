import { projectContainerLog } from "./container-log.collection";
import type { LogEvent } from "@ployz/sdk";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";

// A stream that ends, however it ends, is retried by the browser after `retry`; the viewer never sees it drop.
const RETRY_MS = 3_000;
const OFFLINE_RETRY_MS = 5_000;

const TAIL_BUDGET_MS = 3_000;
// Past this the follow is left unread until `live`: its transport holds the rest, so nothing is lost or piled up here.
const HELD_MAX = 1_000;

type ContainerLogStreamEvent = LogEvent | { type: "live" };

/**
 * `live` follows the tail, so a viewer that sees it with no records knows the log is empty.
 * A tail slower than the budget stops holding everyone back: the rest of it is dropped and `live` goes out anyway.
 * The follow starts with the tail and is held until `live`, so nothing written in between falls through;
 * it repeats the tail, and the rows' ids drop what the viewer already has.
 */
export async function* backfillThenFollow(tail: AsyncIterable<LogEvent>, follow: AsyncIterable<LogEvent>): AsyncIterable<ContainerLogStreamEvent> {
  const tailReader = tail[Symbol.asyncIterator]();
  const followReader = follow[Symbol.asyncIterator]();
  const fromTail = () => tailReader.next().then(result => ({ from: "tail" as const, result }));
  const fromFollow = () => followReader.next().then(result => ({ from: "follow" as const, result }));
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<{ from: "late" }>(resolve => { timer = setTimeout(resolve, TAIL_BUDGET_MS, { from: "late" }); });
  const held: LogEvent[] = [];
  let followDone = false;
  let nextTail = fromTail();
  let nextFollow = fromFollow();
  try {
    for (;;) {
      const next = await Promise.race(followDone || held.length >= HELD_MAX ? [nextTail, late] : [nextTail, nextFollow, late]);
      if (next.from === "late") break;
      if (next.from === "tail") {
        if (next.result.done) break;
        yield next.result.value;
        nextTail = fromTail();
      } else if (next.result.done) followDone = true;
      else {
        held.push(next.result.value);
        nextFollow = fromFollow();
      }
    }
    // Released now, not only in `finally`, so a hung tail doesn't outlive the follow it handed over to.
    // A hung read queues this behind its pending `next()`; the request's abort is what releases it.
    clearTimeout(timer);
    void tailReader.return?.()?.catch(() => {});
    yield { type: "live" };
    yield* held;
    while (!followDone) {
      const { result } = await nextFollow;
      if (result.done) return;
      yield result.value;
      nextFollow = fromFollow();
    }
  } finally {
    clearTimeout(timer);
    void tailReader.return?.()?.catch(() => {});
    void followReader.return?.()?.catch(() => {});
  }
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
