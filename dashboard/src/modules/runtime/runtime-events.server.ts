import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import type { UnreachableReason } from "#/modules/runtime/runtime.collection";
import { type RuntimeConnectionStatusEvent, runtimeWatchFrameForTransport } from "#/modules/runtime/runtime-watch-frame";
import { lacksClusterKey, type PloyzProviderError, predatesSealing, speaksOlderProtocol } from "#/modules/runtime/ployz.server";

export type RuntimeWatch =
  | {
      status: "connected";
      frames: AsyncIterable<RuntimeWatchView>;
      close: () => Promise<void>;
    }
  | {
      status: "no_connection";
    }
  | {
      status: "unreachable";
      error: PloyzProviderError | null;
    };

type RuntimeEventsSource = { request: Request } & RuntimeWatch;

/** Why Cloud can't watch the Cluster. A predated Server's refusal also reads as lacking the key, so it is checked first. */
function unreachableReason(error: PloyzProviderError | null): UnreachableReason {
  if (error === null) return "cluster_unreachable";
  if (speaksOlderProtocol(error)) return "older_relay";
  if (predatesSealing(error)) return "predates_sealing";
  if (lacksClusterKey(error)) return "awaits_key";
  return "cluster_unreachable";
}

/**
 * The `error` text a tab of the build before `reason` matches exactly to pick its view. Remove it once no such
 * tab can still be open, a release after this one.
 */
function legacyError(event: RuntimeConnectionStatusEvent) {
  if (event.status === "no_connection") return null;
  switch (event.reason) {
    case "cluster_unreachable": return "The cluster is expected but unreachable.";
    case "older_relay": return "A Server runs an older Ployz that Ployz Cloud can't reach through the relay.";
    case "predates_sealing":
      return "A Server runs a Ployz older than 0.3.3, which can't show Ployz Cloud that it belongs to your cluster.";
    case "awaits_key":
      return "A Server answers but doesn't hold your cluster's key yet, so Ployz Cloud can't tell it belongs to your cluster.";
  }
}

/**
 * Relay the SDK's watch JSON without projecting it into Cloud-owned runtime
 * semantics. Connection-only outcomes use their own event because they carry
 * no observation at all.
 */
export function createRuntimeEventsResponse(input: RuntimeEventsSource) {
  const encoder = new TextEncoder();
  let cleanupStream = () => undefined;
  let cancelStream: () => Promise<void> = async () => undefined;
  let flushPendingWatch = () => undefined;

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      let closed = false;
      let heartbeatInterval: ReturnType<typeof setInterval> | null = null;
      let pendingWatchEvent: string | null = null;

      const write = (chunk: string) => {
        if (!closed) controller.enqueue(encoder.encode(chunk));
      };

      const writeWatch = (frame: RuntimeWatchView) => {
        const event = `event: runtime.watch\ndata: ${JSON.stringify(
          runtimeWatchFrameForTransport(frame),
        )}\n\n`;
        if ((controller.desiredSize ?? 0) > 0) {
          write(event);
        } else {
          pendingWatchEvent = event;
        }
      };

      const writeStatus = (event: RuntimeConnectionStatusEvent) => {
        write(
          `event: runtime.status\ndata: ${JSON.stringify({ ...event, error: legacyError(event) })}\n\n`,
        );
      };

      flushPendingWatch = () => {
        if (pendingWatchEvent === null || (controller.desiredSize ?? 0) <= 0) {
          return;
        }
        const event = pendingWatchEvent;
        pendingWatchEvent = null;
        write(event);
      };

      const cleanup = async (closeController: boolean) => {
        if (closed) return;
        closed = true;
        pendingWatchEvent = null;
        if (heartbeatInterval) clearInterval(heartbeatInterval);
        input.request.signal.removeEventListener("abort", cleanupStream);
        if (closeController) controller.close();
        if (input.status === "connected") {
          try {
            await input.close();
          } catch (cause) {
            console.error(
              "[runtime] failed to close watch stream",
              new RuntimeConnectionFailure({ cause }),
            );
          }
        }
      };

      cleanupStream = () => {
        void cleanup(true);
      };
      cancelStream = () => cleanup(false);
      input.request.signal.addEventListener("abort", cleanupStream, {
        once: true,
      });

      write(
        `retry: ${
          input.status === "no_connection" || input.status === "unreachable"
            ? 5000
            : 1000
        }\n\n`,
      );
      heartbeatInterval = setInterval(() => {
        // A named event, not an SSE comment: the page watches for it to notice a stream gone silent.
        if ((controller.desiredSize ?? 0) > 0) write("event: ping\ndata: {}\n\n");
      }, 25_000);

      if (input.status === "no_connection") {
        writeStatus({ status: "no_connection" });
        void cleanup(true);
        return;
      }

      if (input.status === "unreachable") {
        writeStatus({ status: "unreachable", reason: unreachableReason(input.error) });
        void cleanup(true);
        return;
      }

      void (async () => {
        try {
          for await (const next of input.frames) writeWatch(next);
        } catch (cause) {
          console.error(
            "[runtime] watch stream failed",
            new RuntimeConnectionFailure({ cause }),
          );
        }
        await cleanup(true);
      })();
    },
    pull() {
      flushPendingWatch();
    },
    cancel() {
      return cancelStream();
    },
  });

  return new Response(stream, {
    headers: {
      "Cache-Control": "no-cache, no-transform",
      Connection: "keep-alive",
      "Content-Type": "text/event-stream",
      "X-Accel-Buffering": "no",
    },
  });
}
