import { projectContainerLog } from "./container-log.collection";
import type { LogEvent } from "@ployz/sdk";
import { eventStreamResponse, sseEvent } from "#/server/event-stream";

/** Pull-driven delivery preserves every log record rather than coalescing watch snapshots. */
export function containerLogResponse(request: Request, events: AsyncIterable<LogEvent>, close: () => Promise<void>) {
  return eventStreamResponse(request.signal, () => containerLogEvents(events), { heartbeatMs: 15_000, onClose: close });
}

async function* containerLogEvents(events: AsyncIterable<LogEvent>) {
  try {
    for await (const event of events) {
      yield sseEvent({ event: "log", data: event.type === "record" ? { type: "record", record: projectContainerLog(event.record) } : event });
    }
  } catch {
    yield sseEvent({ event: "unavailable", data: {} });
  }
}
