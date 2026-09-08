import { describe, expect, it, vi } from "vitest";
import { PloyzProviderError } from "#/modules/runtime/ployz.server";
import { CLUSTER_UNREACHABLE_ERROR } from "#/modules/runtime/runtime.collection";
import { createRuntimeEventsResponse } from "#/modules/runtime/runtime-events.server";
import {
  runtimeWatchCertificateFixture,
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

const OBSERVED_AT = "2026-08-18T00:00:00.000Z";

describe("createRuntimeEventsResponse", () => {
  it("streams a redacted Runtime Watch observation without inventing a lens", async () => {
    const close = vi.fn(async () => undefined);
    const secret = "postgres://runtime-secret@example.test/app";
    const container = runtimeWatchContainerFixture("machine-a", "ctr-1");
    const certificate = runtimeWatchCertificateFixture("api.example.test");
    container.resolved_spec.container.environment = { DATABASE_URL: secret };
    const watchFrame = runtimeWatchFrameFixture({
      observed_at: OBSERVED_AT,
      machines: [
        runtimeWatchMachineObservationFixture({
          machine: runtimeWatchMachineFixture("machine-a", "edge-a"),
          membership: "up",
        }),
      ],
      containers: [container],
      services: [
        {
          identity: "production/api",
          service_id: container.resolved_spec.service_id,
          containers: [container],
          hook_containers: [],
        },
      ],
      certificates: [certificate],
      incomplete_ids: {
        machines: [],
        containers: [],
        volumes: [],
        certificates: [certificate.hostname],
      },
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
    const event = new TextDecoder().decode(second?.value);

    expect(new TextDecoder().decode(first?.value)).toBe("retry: 1000\n\n");
    expect(event).toContain("event: runtime.watch\n");
    expect(event).toContain('"observed_at":"2026-08-18T00:00:00.000Z"');
    expect(event).toContain('"identity":"production/api"');
    expect(event).toContain('"hostname":"api.example.test"');
    expect(event).not.toContain(secret);
    expect(event).not.toContain("resolved_spec");
    expect(event).not.toContain('"environment"');
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
      '"observed_at":"1970-01-01T00:00:03.000Z"',
    );
    await reader?.cancel();
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

    expect(statusText).toContain("event: runtime.status\n");
    expect(statusText).toContain('"status":"no_connection"');
    expect(await reader?.read()).toMatchObject({ done: true });
  });

  it("closes an unreachable stream with a connection-only status", async () => {
    const response = createRuntimeEventsResponse({
      request: new Request("http://localhost/api/runtime/events"),
      status: "unreachable",
      error: null,
    });
    const reader = response.body?.getReader();

    await reader?.read();
    const statusChunk = await reader?.read();
    const statusText = new TextDecoder().decode(statusChunk?.value);

    expect(statusText).toContain("event: runtime.status\n");
    expect(statusText).toContain('"status":"unreachable"');
    expect(statusText).not.toContain('"machines"');
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
