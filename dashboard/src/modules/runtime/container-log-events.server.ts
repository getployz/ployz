import { projectContainerLog } from "./container-log.collection";
import type { LogEvent } from "@ployz/sdk";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";

// A stream that ends, however it ends, is retried by the browser after `retry`; the viewer never sees it drop.
const RETRY_MS = 3_000;
const OFFLINE_RETRY_MS = 5_000;

/** Pull-driven delivery preserves every log record rather than coalescing watch snapshots. `live` opens the stream. */
export function containerLogResponse(request: Request, events: AsyncIterable<LogEvent>, close: () => Promise<void>) {
  return eventStreamResponse(request.signal, () => containerLogEvents(events), { heartbeatMs: 15_000, retryMs: RETRY_MS, onClose: close });
}

/** The organization's servers are unreachable: say so, then let the browser ask again. */
export function offlineLogResponse(request: Request) {
  return eventStreamResponse(request.signal, async function* () { yield sseEvent({ event: "offline", data: {} }); }, { heartbeatMs: 15_000, retryMs: OFFLINE_RETRY_MS });
}

async function* containerLogEvents(events: AsyncIterable<LogEvent>) {
  yield sseEvent({ event: "live", data: {} });
  try {
    for await (const event of events) {
      yield sseEvent({ event: "log", data: event.type === "record" ? { type: "record", record: projectContainerLog(event.record) } : event });
    }
  } catch {
    // The upstream broke; ending the stream makes the browser reconnect.
  }
}
