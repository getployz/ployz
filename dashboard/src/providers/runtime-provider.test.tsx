// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { getDbClient } from "#/collections/scope";
import type { ContainerId } from "@ployz/sdk";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { runtimeWatchFrameForTransport } from "#/modules/runtime/runtime-watch-frame";
import {
  RuntimeProvider,
  useRuntimeService,
  useRuntimeStatus,
} from "./runtime-provider";

type RuntimeEventListener = (event: MessageEvent) => void;

const eventSources: FakeEventSource[] = [];
const clients: QueryClient[] = [];

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

afterEach(async () => {
  cleanup();
  for (const client of clients.splice(0)) { await getDbClient(client).cleanup(); client.clear(); }
  eventSources.length = 0;
});

function latestEventSource() {
  const eventSource = eventSources.at(-1);
  if (!eventSource) throw new Error("Expected RuntimeProvider to open EventSource.");
  return eventSource;
}

function Probe({ identity = "production/api" }: { identity?: string }) {
  const runtimeStatus = useRuntimeStatus();
  const { runtime } = useRuntimeService(identity);

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

function LensProbe({ organizationSlug }: { organizationSlug: string }) {
  const runtime = useRuntimeLens(organizationSlug);
  return <output data-testid="lens">{runtime.status}:{runtime.machines.length}</output>;
}

async function renderRuntime(children: ReactNode) {
  const client = new QueryClient();
  clients.push(client);
  const root = createRootRoute();
  const protectedRoute = createRoute({
    getParentRoute: () => root, id: "_protected",
    beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }),
  });
  const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([index])]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  await router.load();
  return render(children, {
    wrapper: ({ children }) => <QueryClientProvider client={client}><RouterContextProvider router={router}>{children}</RouterContextProvider></QueryClientProvider>,
  });
}

async function renderProvider(organizationSlug: string) {
  await renderRuntime(
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
    const eventSource = await renderProvider("runtime-provider-observed");

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

  it.each(["runtime.watch", "error"])("retains the last observation when %s fails", async (eventType) => {
    const eventSource = await renderProvider(`runtime-provider-failure-${eventType}`);

    act(() => {
      eventSource.emit("runtime.watch", JSON.stringify(watchFrame()));
    });
    await waitFor(() => {
      expect(screen.getByTestId("runtime").getAttribute("data-status")).toBe(
        "observed",
      );
    });

    act(() => {
      eventSource.emit(eventType, JSON.stringify({ services: [] }));
    });

    await waitFor(() => {
      const output = screen.getByTestId("runtime");
      expect(output.getAttribute("data-status")).toBe("unavailable");
      expect(output.getAttribute("data-containers")).toBe("1");
    });
  });

  it("retains each organization's cached observation across switches and connection errors", async () => {
    const view = (organizationSlug: string) => (
      <RuntimeProvider organizationSlug={organizationSlug}>
        <Probe />
        <LensProbe organizationSlug={organizationSlug} />
      </RuntimeProvider>
    );
    const { rerender } = await renderRuntime(view("runtime-switch-a"));
    const firstSource = latestEventSource();
    act(() => firstSource.emit("runtime.watch", JSON.stringify(watchFrame())));
    await waitFor(() => {
      expect(screen.getByTestId("runtime").getAttribute("data-containers")).toBe("1");
    });

    rerender(view("runtime-switch-b"));
    act(() => latestEventSource().emit("error", ""));
    await waitFor(() => {
      const output = screen.getByTestId("runtime");
      expect(output.getAttribute("data-status")).toBe("unavailable");
      expect(output.getAttribute("data-containers")).toBe("none");
      expect(screen.getByTestId("lens").textContent).toBe("unavailable:0");
    });

    rerender(view("runtime-switch-a"));
    act(() => latestEventSource().emit("error", ""));
    await waitFor(() => {
      const output = screen.getByTestId("runtime");
      expect(output.getAttribute("data-status")).toBe("unavailable");
      expect(output.getAttribute("data-containers")).toBe("1");
      expect(output.getAttribute("data-incomplete-containers")).toBe("1");
      expect(screen.getByTestId("lens").textContent).toBe("unavailable:1");
    });
  });

  it("updates the service subscription when its identity changes without remounting", async () => {
    const view = (identity: string) => (
      <RuntimeProvider organizationSlug="runtime-service-switch">
        <Probe identity={identity} />
      </RuntimeProvider>
    );
    const { rerender } = await renderRuntime(view("production/api"));
    act(() => latestEventSource().emit("runtime.watch", JSON.stringify(watchFrame())));
    await waitFor(() => {
      expect(screen.getByTestId("runtime").getAttribute("data-containers")).toBe("1");
    });
    rerender(view("production/worker"));
    await waitFor(() => {
      expect(screen.getByTestId("runtime").getAttribute("data-containers")).toBe("none");
    });
    rerender(view("production/api"));
    await waitFor(() => {
      expect(screen.getByTestId("runtime").getAttribute("data-containers")).toBe("1");
    });
  });
});
