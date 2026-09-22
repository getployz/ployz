import { projectContainerLog } from "./container-log.collection";
import type { LogEvent } from "@ployz/sdk";

/** Pull-driven delivery preserves every log record rather than coalescing watch snapshots. */
export function containerLogResponse(request: Request, events: AsyncIterable<LogEvent>, close: () => Promise<void>) {
  const iterator = events[Symbol.asyncIterator]();
  const encoder = new TextEncoder();
  let finished = false;
  const finish = async () => {
    if (finished) return;
    finished = true;
    request.signal.removeEventListener("abort", abort);
    await close();
    await iterator.return?.();
  };
  const abort = () => { void finish(); };
  request.signal.addEventListener("abort", abort, { once: true });
  const next = () => iterator.next().then(value => ({ value }), error => ({ error }));
  let pending = next();
  const body = new ReadableStream<Uint8Array>({
    async pull(controller) {
      let timer: ReturnType<typeof setTimeout> | undefined;
      try {
        const result = await Promise.race([pending, new Promise<null>(resolve => { timer = setTimeout(() => resolve(null), 15000); })]);
        if (finished) { controller.close(); return; }
        if (result === null) { controller.enqueue(encoder.encode(": heartbeat\n\n")); return; }
        if ("error" in result) throw result.error;
        if (result.value.done) { await finish(); controller.close(); return; }
        controller.enqueue(encoder.encode(`event: log\ndata: ${JSON.stringify(result.value.value.type === "record" ? { type: "record", record: projectContainerLog(result.value.value.record) } : result.value.value)}\n\n`));
        pending = next();
      } catch {
        if (!finished) controller.enqueue(encoder.encode('event: unavailable\ndata: {}\n\n'));
        await finish(); controller.close();
      } finally { clearTimeout(timer); }
    },
    cancel: finish,
  });
  return new Response(body, { headers: { "Content-Type": "text/event-stream", "Cache-Control": "private, no-store, no-transform", "X-Accel-Buffering": "no" } });
}
