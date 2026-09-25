/** One server-sent event frame. */
export function sseEvent(input: { event: string; data: object }) {
  return `event: ${input.event}\ndata: ${JSON.stringify(input.data)}\n\n`;
}

/**
 * Serves server-sent events as they are pulled from `events`, so a slow client applies backpressure.
 * `events` gets a signal that aborts when the request ends or the client cancels; the stream then
 * ends at once, even while `events` waits on a hung upstream. When no event arrives for `heartbeatMs`,
 * a `: ping` comment keeps proxies from closing the idle stream; the pending event carries over.
 *
 * `onClose` runs exactly once however the stream ends. It lives here, not in a generator's `finally`:
 * `return()` on a generator that never started skips its `finally`, and on one awaiting a hung upstream
 * it waits for that upstream forever.
 */
export function eventStreamResponse(
  requestSignal: AbortSignal,
  events: (signal: AbortSignal) => AsyncIterable<string>,
  options: { heartbeatMs: number; retryMs?: number; onClose?: () => Promise<void> },
) {
  const cancelled = new AbortController();
  const signal = AbortSignal.any([requestSignal, cancelled.signal]);
  const iterator = events(signal)[Symbol.asyncIterator]();
  const aborted = new Promise<"aborted">((resolve) => signal.addEventListener("abort", () => resolve("aborted"), { once: true }));
  let finished: Promise<void> | undefined;
  const finish = () => finished ??= (async () => {
    void iterator.return?.()?.catch(() => {});
    await options.onClose?.();
  })();
  signal.addEventListener("abort", () => void finish(), { once: true });
  const encoder = new TextEncoder();
  let pending: Promise<IteratorResult<string>> | undefined;
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      if (options.retryMs !== undefined) controller.enqueue(encoder.encode(`retry: ${options.retryMs}\n\n`));
    },
    async pull(controller) {
      pending ??= iterator.next();
      let timer: ReturnType<typeof setTimeout> | undefined;
      const heartbeat = new Promise<"heartbeat">((resolve) => {
        timer = setTimeout(() => resolve("heartbeat"), options.heartbeatMs);
      });
      const next = await Promise.race([pending, heartbeat, aborted]).finally(() => clearTimeout(timer));
      if (next === "heartbeat") {
        controller.enqueue(encoder.encode(": ping\n\n"));
        return;
      }
      pending = undefined;
      if (next === "aborted" || next.done) {
        await finish();
        if (!cancelled.signal.aborted) controller.close();
        return;
      }
      controller.enqueue(encoder.encode(next.value));
    },
    async cancel() {
      cancelled.abort();
      await finish();
    },
  });
  return new Response(body, {
    headers: {
      "Cache-Control": "private, no-store, no-transform",
      "Content-Type": "text/event-stream",
      "X-Accel-Buffering": "no",
    },
  });
}
