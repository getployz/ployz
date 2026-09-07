import "@tanstack/react-start/server-only";
import type { RuntimeWatchView } from "@ployz/sdk";
import { RuntimeConnectionFailure } from "#/modules/runtime/runtime-connection-errors";
import { Result } from "effect";
import type { PloyzProviderError } from "#/modules/runtime/ployz.server";
import {
  CLUSTER_UNREACHABLE_ERROR,
  RUNTIME_PUBLIC_URL_NONE,
  unreachableRuntimeSnapshot,
  type RuntimeSnapshotLens,
} from "#/modules/runtime/runtime.collection";
import { runtimeSnapshotLensFromWatchFrame } from "#/modules/runtime/runtime-watch-frame";

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

export function createRuntimeEventsResponse(input: RuntimeEventsSource) {
  const encoder = new TextEncoder();
  let cleanupStream = () => undefined;
  let cancelStream: () => Promise<void> = async () => undefined;
  let flushPendingLens = () => undefined;

  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      let closed = false;
      let heartbeatInterval: ReturnType<typeof setInterval> | null = null;
      let pendingLensEvent: string | null = null;

      const write = (chunk: string) => {
        if (!closed) {
          controller.enqueue(encoder.encode(chunk));
        }
      };

      const writeLens = (frame: RuntimeWatchView) => {
        const lens = runtimeSnapshotLensFromWatchFrame(frame);
        if (Result.isFailure(lens)) {
          throw lens.failure;
        }
        const event = `event: runtime.lens\ndata: ${JSON.stringify(lens.success)}\n\n`;
        if ((controller.desiredSize ?? 0) > 0) {
          write(event);
        } else {
          pendingLensEvent = event;
        }
      };

      flushPendingLens = () => {
        if (pendingLensEvent === null || (controller.desiredSize ?? 0) <= 0) {
          return;
        }
        const event = pendingLensEvent;
        pendingLensEvent = null;
        write(event);
      };

      const cleanup = async (closeController: boolean) => {
        if (closed) {
          return;
        }
        closed = true;
        pendingLensEvent = null;
        if (heartbeatInterval) {
          clearInterval(heartbeatInterval);
        }
        input.request.signal.removeEventListener("abort", cleanupStream);
        if (closeController) {
          controller.close();
        }
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
        if ((controller.desiredSize ?? 0) > 0) {
          write(": keepalive\n\n");
        }
      }, 25_000);

      if (input.status === "no_connection") {
        const status: RuntimeSnapshotLens = {
          status: "no_connection",
          error: null,
          publicUrl: RUNTIME_PUBLIC_URL_NONE,
          machines: [],
          services: [],
          updatedAt: new Date().toISOString(),
        };
        write(`event: runtime.lens\ndata: ${JSON.stringify(status)}\n\n`);
        void cleanup(true);
        return;
      }

      if (input.status === "unreachable") {
        const status = unreachableRuntimeSnapshot(
          input.error?.message || CLUSTER_UNREACHABLE_ERROR,
        );
        write(`event: runtime.lens\ndata: ${JSON.stringify(status)}\n\n`);
        void cleanup(true);
        return;
      }

      void (async () => {
        try {
            for await (const next of input.frames) {
              writeLens(next);
            }
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
      flushPendingLens();
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
