/**
 * Why a stream stopped delivering: an error the browser will retry, a refusal (the server answered with an error, so
 * the browser gave up and this retries with backoff), a minute with nothing from a server that pings, or the browser
 * going offline. A loss is reported once; the next connection's `open` ends it.
 */
export type StreamLoss = "error" | "refused" | "silent" | "offline";

export type LiveStreamOptions = {
  /** Named server events, by name. Every event, `ping` included, proves the stream is live. */
  on: Record<string, (event: MessageEvent<string>) => void>;
  onOpen?: () => void;
  onLost?: (why: StreamLoss) => void;
  /** Silence that counts as lost. The server pings every 15–25s. */
  silenceMs?: number;
};

const MAX_RETRY_MS = 30_000;

/**
 * The one way the dashboard follows a server-sent event stream. A dropped connection often never errors (a sleeping
 * laptop, a Wi-Fi handover), so liveness is the stream's job, not each caller's: it listens for the server's `ping`,
 * treats silence or the browser going offline as a loss, and opens a fresh connection. Returns `close`.
 */
export function liveStream(url: string, { on, onOpen, onLost, silenceMs = 60_000 }: LiveStreamOptions): () => void {
  // Server-rendered pages never stream.
  if (import.meta.env.SSR) return () => {};
  let source: EventSource | undefined;
  let heard = Date.now();
  let lost = false;
  let retryMs = 1_000;
  let retry: ReturnType<typeof setTimeout> | undefined;

  const lose = (why: StreamLoss) => {
    if (lost) return;
    lost = true;
    onLost?.(why);
  };
  const live = () => {
    heard = Date.now();
    lost = false;
    retryMs = 1_000;
  };

  const connect = () => {
    source?.close();
    clearTimeout(retry);
    heard = Date.now();
    const next = new EventSource(url);
    source = next;
    next.addEventListener("open", () => { live(); onOpen?.(); });
    next.addEventListener("ping", live);
    for (const [name, handler] of Object.entries(on)) {
      next.addEventListener(name, (event) => {
        live();
        // SAFETY: EventSource dispatches every named server event as a MessageEvent whose data is the event's text.
        handler(event as MessageEvent<string>);
      });
    }
    next.addEventListener("error", () => {
      // The browser retries a dropped stream by itself; only a refused one (an error response) closes for good.
      if (next.readyState !== EventSource.CLOSED) return lose("error");
      lose("refused");
      retry = setTimeout(connect, retryMs);
      retryMs = Math.min(retryMs * 2, MAX_RETRY_MS);
    });
  };

  const watchdog = setInterval(() => {
    if (Date.now() - heard < silenceMs) return;
    lose("silent");
    connect();
  }, Math.min(10_000, silenceMs));
  const offline = () => lose("offline");
  const online = () => connect();

  window.addEventListener("offline", offline);
  window.addEventListener("online", online);
  connect();

  return () => {
    clearInterval(watchdog);
    clearTimeout(retry);
    window.removeEventListener("offline", offline);
    window.removeEventListener("online", online);
    source?.close();
  };
}
