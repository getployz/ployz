import { projectContainerLog } from "./container-log.collection";
import type { LogEvent } from "@ployz/sdk";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";

// A stream that ends, however it ends, is retried by the browser after `retry`; the viewer never sees it drop.
const RETRY_MS = 3_000;
const OFFLINE_RETRY_MS = 5_000;

const TAIL_BUDGET_MS = 3_000;
// Past this the follow is left unread until `live`: its transport holds the rest, so nothing is lost or piled up here.
const HELD_MAX = 1_000;

type ContainerLogStreamEvent = LogEvent | { type: "live" } | { type: "hidden" };

/** How often an open stream rereads what the Organization shows Ployz support. */
const RECHECK_MS = 5_000;

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
  const fromFollow = () => {
    const read = followReader.next().then(result => ({ from: "follow" as const, result }));
    // A read parked at HELD_MAX sits outside any race until `live`; awaiting it then still throws.
    read.catch(() => {});
    return read;
  };
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

/**
 * Support's stream, closed once the Organization hides its container logs: what it shows is read again every
 * RECHECK_MS while lines flow. A failed read ends the stream, and the browser's reconnect authorizes afresh.
 */
export async function* whileLogsShown(events: AsyncIterable<ContainerLogStreamEvent>, shown: () => Promise<boolean>, recheckMs = RECHECK_MS): AsyncIterable<ContainerLogStreamEvent> {
  const reader = events[Symbol.asyncIterator]();
  let next = reader.next();
  // A read parked across a recheck that ends the stream still throws when `return` releases it.
  next.catch(() => {});
  let checked = Date.now();
  try {
    for (;;) {
      // Checked before each line, not only when the timer wins: a steady flow of lines would always beat it.
      if (Date.now() >= checked + recheckMs) {
        const still = await shown().catch(() => undefined);
        if (still === undefined) return;
        if (!still) {
          yield { type: "hidden" };
          return;
        }
        checked = Date.now();
      }
      let timer: ReturnType<typeof setTimeout> | undefined;
      const due = new Promise<"due">(resolve => { timer = setTimeout(resolve, checked + recheckMs - Date.now(), "due"); });
      const won = await Promise.race([next, due]).finally(() => clearTimeout(timer));
      if (won === "due") continue;
      if (won.done) return;
      yield won.value;
      next = reader.next();
      next.catch(() => {});
    }
  } finally {
    void reader.return?.()?.catch(() => {});
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

/** The Organization hides its container logs from Ployz support: say so once; the viewer stops asking. */
export function hiddenLogResponse(request: Request) {
  return eventStreamResponse(request.signal, async function* () { yield sseEvent({ event: "hidden", data: {} }); }, { heartbeatMs: 15_000 });
}

async function* containerLogEvents(events: AsyncIterable<ContainerLogStreamEvent>) {
  try {
    for await (const event of events) {
      if (event.type === "live") yield sseEvent({ event: "live", data: {} });
      else if (event.type === "hidden") yield sseEvent({ event: "hidden", data: {} });
      else yield sseEvent({ event: "log", data: event.type === "record" ? { type: "record", record: projectContainerLog(event.record) } : event });
    }
  } catch {
    // The upstream broke; ending the stream makes the browser reconnect.
  }
}
