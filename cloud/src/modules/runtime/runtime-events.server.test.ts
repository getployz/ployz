import { describe, expect, it, vi } from "vitest";
import { PloyzProviderError } from "#/modules/runtime/ployz.server";
import { CLUSTER_UNREACHABLE_ERROR } from "#/modules/runtime/runtime.collection";
import { createRuntimeEventsResponse } from "#/modules/runtime/runtime-events.server";
import {
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

const OBSERVED_AT = "2026-08-18T00:00:00.000Z";

describe("createRuntimeEventsResponse", () => {
  it("projects a watch frame's machine testimony into a lens event", async () => {
    const close = vi.fn(async () => undefined);
    const watchFrame = runtimeWatchFrameFixture({
      observed_at: OBSERVED_AT,
      machines: [
        runtimeWatchMachineObservationFixture({
          machine: runtimeWatchMachineFixture("machine-a", "edge-a"),
          membership: "up",
        }),
      ],
      containers: [runtimeWatchContainerFixture("machine-a", "ctr-1")],
    });

    async function* frames() {
      yield watchFrame;
      await new Promise(() => undefined);
    }

    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "connected",
      frames: frames(),
      close,
    });
    const reader = response.body?.getReader();

    const first = await reader?.read();
    const second = await reader?.read();

    expect(new TextDecoder().decode(first?.value)).toBe("retry: 1000\n\n");
    const event = new TextDecoder().decode(second?.value);
    expect(event).toContain("event: runtime.lens\n");
    expect(event).toContain('"observedContainerCount":1');
    expect(event).not.toContain('"observed_container_count"');
    expect(response.headers.get("content-type")).toBe("text/event-stream");

    await reader?.cancel();
    expect(close).toHaveBeenCalledOnce();
  });

  it("keeps only the latest watch frame while the browser is backpressured", async () => {
    let resolvePumped: () => void = () => undefined;
    const pumped = new Promise<void>((resolve) => {
      resolvePumped = resolve;
    });
    async function* frames() {
      yield runtimeWatchFrameFixture({ observed_at: "1970-01-01T00:00:01.000Z" });
      yield runtimeWatchFrameFixture({ observed_at: "1970-01-01T00:00:02.000Z" });
      yield runtimeWatchFrameFixture({ observed_at: "1970-01-01T00:00:03.000Z" });
      resolvePumped();
      await new Promise(() => undefined);
    }

    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "connected",
      frames: frames(),
      close: async () => undefined,
    });
    const reader = response.body?.getReader();
    await pumped;
    await reader?.read();
    const event = await reader?.read();

    expect(new TextDecoder().decode(event?.value)).toContain(
      '"updatedAt":"1970-01-01T00:00:03.000Z"',
    );
    await reader?.cancel();
  });

  it("closes the stream when watch-frame projection fails", async () => {
    const close = vi.fn(async () => undefined);
    const log = vi.spyOn(console, "error").mockImplementation(() => undefined);
    async function* frames() {
      yield runtimeWatchFrameFixture({
        observed_at: OBSERVED_AT,
        machines: [
          runtimeWatchMachineObservationFixture({
            machine: runtimeWatchMachineFixture("", "edge-a"),
          }),
        ],
      });
    }
    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "connected",
      frames: frames(),
      close,
    });
    const reader = response.body?.getReader();

    await reader?.read();
    expect(await reader?.read()).toMatchObject({ done: true });
    expect(close).toHaveBeenCalledOnce();
    log.mockRestore();
  });

  it("handles client cancellation after the upstream finishes", async () => {
    let releaseClose: () => void = () => undefined;
    const closeBlocked = new Promise<void>((resolve) => {
      releaseClose = resolve;
    });
    const close = vi.fn(() => closeBlocked);
    async function* frames() {
      yield* [];
    }
    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "connected",
      frames: frames(),
      close,
    });
    const reader = response.body?.getReader();

    await reader?.read();
    await vi.waitFor(() => expect(close).toHaveBeenCalledOnce());
    const cancel = reader?.cancel();
    releaseClose();
    await cancel;
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(close).toHaveBeenCalledOnce();
  });

  it("handles upstream completion after client cancellation", async () => {
    let finishFrames: () => void = () => undefined;
    const framesBlocked = new Promise<void>((resolve) => {
      finishFrames = resolve;
    });
    const close = vi.fn(async () => undefined);
    async function* frames() {
      await framesBlocked;
      yield* [];
    }
    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "connected",
      frames: frames(),
      close,
    });
    const reader = response.body?.getReader();

    await reader?.read();
    await reader?.cancel();
    finishFrames();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(close).toHaveBeenCalledOnce();
  });

  it("treats repeated cleanup as normal teardown", async () => {
    const abortController = new AbortController();
    let finishFrames: () => void = () => undefined;
    const framesBlocked = new Promise<void>((resolve) => {
      finishFrames = resolve;
    });
    let releaseClose: () => void = () => undefined;
    const closeBlocked = new Promise<void>((resolve) => {
      releaseClose = resolve;
    });
    const close = vi.fn(() => closeBlocked);
    async function* frames() {
      await framesBlocked;
      yield* [];
    }
    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events", {
        signal: abortController.signal,
      }),
      status: "connected",
      frames: frames(),
      close,
    });
    const reader = response.body?.getReader();

    await reader?.read();
    abortController.abort();
    await vi.waitFor(() => expect(close).toHaveBeenCalledOnce());
    finishFrames();
    const cancel = reader?.cancel();
    releaseClose();
    await cancel;
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(close).toHaveBeenCalledOnce();
  });

  it("closes a no-connection stream so EventSource can retry", async () => {
    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "no_connection",
    });
    const reader = response.body?.getReader();

    await reader?.read();
    const statusChunk = await reader?.read();
    const statusText = new TextDecoder().decode(statusChunk?.value);

    expect(statusText).toContain("event: runtime.lens\n");
    expect(statusText).toContain('"status":"no_connection"');
    expect(await reader?.read()).toMatchObject({ done: true });
  });

  it("closes an unreachable stream with empty machines so stale rows are not membership", async () => {
    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "unreachable",
      error: null,
    });
    const reader = response.body?.getReader();

    await reader?.read();
    const statusChunk = await reader?.read();
    const statusText = new TextDecoder().decode(statusChunk?.value);

    expect(statusText).toContain("event: runtime.lens\n");
    expect(statusText).toContain('"status":"unreachable"');
    expect(statusText).toContain('"machines":[]');
    expect(await reader?.read()).toMatchObject({ done: true });
  });

  it("falls back to the cluster unreachable copy when the provider error has no message", async () => {
    const providerError = new PloyzProviderError({
      operation: "connect",
      cause: new Error("dial refused"),
    });
    expect(providerError.message).toBe("");
    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "unreachable",
      error: providerError,
    });
    const reader = response.body?.getReader();

    await reader?.read();
    const statusChunk = await reader?.read();
    const statusText = new TextDecoder().decode(statusChunk?.value);

    expect(statusText).toContain(
      `"error":${JSON.stringify(CLUSTER_UNREACHABLE_ERROR)}`,
    );
  });
});
