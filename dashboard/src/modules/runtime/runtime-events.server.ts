import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import { CLUSTER_UNREACHABLE_ERROR } from "#/modules/runtime/runtime.collection";
import { runtimeWatchFrameForTransport } from "#/modules/runtime/runtime-watch-frame";
import type { PloyzProviderError } from "#/modules/runtime/ployz.server";

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

      const writeStatus = (status: "no_connection" | "unreachable", error: string | null) => {
        write(
          `event: runtime.status\ndata: ${JSON.stringify({ status, error })}\n\n`,
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
        if ((controller.desiredSize ?? 0) > 0) write(": keepalive\n\n");
      }, 25_000);

      if (input.status === "no_connection") {
        writeStatus("no_connection", null);
        void cleanup(true);
        return;
      }

      if (input.status === "unreachable") {
        writeStatus(
          "unreachable",
          input.error?.message || CLUSTER_UNREACHABLE_ERROR,
        );
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
