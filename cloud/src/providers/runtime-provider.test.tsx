// @vitest-environment jsdom

import type { ContainerId } from "@ployz/sdk";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { runtimeWatchFrameForTransport } from "#/modules/runtime/runtime-watch-frame";
import {
  RuntimeProvider,
  useRuntimeService,
  useRuntimeStatus,
} from "./runtime-provider";

type RuntimeEventListener = (event: MessageEvent) => void;

const eventSources: FakeEventSource[] = [];

class FakeEventSource {
  private readonly listeners = new Map<string, RuntimeEventListener>();

  constructor(_url: string) {
    eventSources.push(this);
  }

  addEventListener(type: string, listener: RuntimeEventListener) {
    this.listeners.set(type, listener);
  }

  removeEventListener(type: string, listener: RuntimeEventListener) {
    if (this.listeners.get(type) === listener) this.listeners.delete(type);
  }

  close() {}

  emit(type: string, data: string) {
    this.listeners.get(type)?.(new MessageEvent(type, { data }));
  }
}

vi.stubGlobal("EventSource", FakeEventSource);

afterEach(() => {
  cleanup();
  eventSources.length = 0;
});

function latestEventSource() {
  const eventSource = eventSources.at(-1);
  if (!eventSource) throw new Error("Expected RuntimeProvider to open EventSource.");
  return eventSource;
}

function Probe() {
  const runtimeStatus = useRuntimeStatus();
  const { runtime } = useRuntimeService("production/api");

  return (
    <output
      data-testid="runtime"
      data-status={runtimeStatus.lensStatus}
      data-containers={runtime ? String(runtime.containers.length) : "none"}
      data-incomplete-containers={String(
        runtimeStatus.incompleteIds.containers.length,
      )}
    />
  );
}

function renderProvider(organizationSlug: string) {
  render(
    <RuntimeProvider organizationSlug={organizationSlug}>
      <Probe />
    </RuntimeProvider>,
  );
  return latestEventSource();
}

function watchFrame() {
  const container = runtimeWatchContainerFixture("machine-a", "container-a");
  return runtimeWatchFrameForTransport(
    runtimeWatchFrameFixture({
      machines: [
        runtimeWatchMachineObservationFixture({
          machine: runtimeWatchMachineFixture("machine-a", "edge-a"),
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
      incomplete_ids: {
        machines: [],
        containers: ["container-missing" as ContainerId],
        volumes: [],
        certificates: [],
      },
    }),
  );
}

describe("RuntimeProvider", () => {
  it("decodes a direct Runtime Watch event and clears it for an unreachable status", async () => {
    const eventSource = renderProvider("runtime-provider-observed");

    act(() => {
      eventSource.emit("runtime.watch", JSON.stringify(watchFrame()));
    });

    await waitFor(() => {
      const output = screen.getByTestId("runtime");
      expect(output.getAttribute("data-status")).toBe("observed");
      expect(output.getAttribute("data-containers")).toBe("1");
      expect(output.getAttribute("data-incomplete-containers")).toBe("1");
    });

    act(() => {
      eventSource.emit(
        "runtime.status",
        JSON.stringify({ status: "unreachable", error: "dial failed" }),
      );
    });

    await waitFor(() => {
      const output = screen.getByTestId("runtime");
      expect(output.getAttribute("data-status")).toBe("unreachable");
      expect(output.getAttribute("data-containers")).toBe("none");
      expect(output.getAttribute("data-incomplete-containers")).toBe("0");
    });
  });

  it("marks malformed Runtime Watch data unavailable while retaining the last direct observation", async () => {
    const eventSource = renderProvider("runtime-provider-malformed");

    act(() => {
      eventSource.emit("runtime.watch", JSON.stringify(watchFrame()));
    });
    await waitFor(() => {
      expect(screen.getByTestId("runtime").getAttribute("data-status")).toBe(
        "observed",
      );
    });

    act(() => {
      eventSource.emit("runtime.watch", JSON.stringify({ services: [] }));
    });

    await waitFor(() => {
      const output = screen.getByTestId("runtime");
      expect(output.getAttribute("data-status")).toBe("unavailable");
      expect(output.getAttribute("data-containers")).toBe("1");
    });
  });
});
