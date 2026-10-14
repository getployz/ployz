// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { serverStatus } from "#/modules/machines/server-status";
import * as functions from "#/modules/metrics/metrics.functions";
import * as model from "#/modules/metrics/metrics-model";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { ServerMeters } from "./server-meters";
import * as now from "./server-now";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const readServerMetrics = () => vi.spyOn(functions, "readServerMetricsServerFn").mockReturnValue(new Promise(() => {}) as never);

/** The list's container query shows the meter cell (some width) or hides it (none). */
function viewport({ wide }: { wide: boolean }) {
  vi.stubGlobal("ResizeObserver", class {
    constructor(private readonly report: ResizeObserverCallback) {}
    observe() { this.report([{ contentRect: { width: wide ? 304 : 0 } } as ResizeObserverEntry], this as never); }
    disconnect() {}
  });
}

function showRow(membership: RuntimeMachineRecord["membership"]) {
  const machine: RuntimeMachineRecord = {
    id: "1".repeat(32), name: "syd-1", publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: true, acceptsServices: true, storage: "pool",
    buildConcurrency: null, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.3.0", capabilities: [], membership,
    observedContainerCount: 0, observedAt: "2026-10-12T00:00:00Z",
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const view = render(
    <QueryClientProvider client={client}>
      <ServerMeters organizationSlug="acme" server={{ machine, name: machine.name, status: serverStatus(machine) }} />
    </QueryClientProvider>,
  );
  return { ...view, client };
}

it("shows nothing for an offline Server and never dials it", () => {
  const reads = readServerMetrics();
  viewport({ wide: true });
  expect(showRow("down").container.textContent).toBe("");
  expect(reads).not.toHaveBeenCalled();
});

it("shows nothing while the read is pending or after it fails", async () => {
  viewport({ wide: true });
  readServerMetrics();
  expect(showRow("up").container.textContent).toBe("");
  cleanup();

  vi.spyOn(functions, "readServerMetricsServerFn").mockRejectedValue(new Error("boom"));
  const { container, client } = showRow("up");
  await waitFor(() => expect(client.getQueryCache().getAll()[0]?.state.status).toBe("error"));
  expect(container.textContent).toBe("");
  expect(screen.queryByText(/answer|readings|Upgrade/)).toBeNull();
});

it("reads an online Server only when its meters show", () => {
  const reads = readServerMetrics();
  viewport({ wide: false });
  showRow("up");
  expect(reads).not.toHaveBeenCalled();

  viewport({ wide: true });
  showRow("up");
  expect(reads).toHaveBeenCalledTimes(1);
});

it("shows CPU and memory, and stays empty when only a disk reading is fresh", async () => {
  viewport({ wide: true });
  vi.spyOn(functions, "readServerMetricsServerFn").mockResolvedValue({} as never);
  vi.spyOn(model, "serverMetrics").mockReturnValue({} as never);
  const reads = vi.spyOn(now, "serverNow").mockReturnValue({ type: "meters", cpu: 0.47, memory: null, disk: null });
  const shown = showRow("up");
  await screen.findByText("CPU");
  expect(shown.container.textContent).toMatch(/^CPU47%.*Memory–$/);
  cleanup();

  reads.mockReturnValue({ type: "meters", cpu: null, memory: null, disk: { used: 23, total: 100 } });
  const { container, client } = showRow("up");
  await waitFor(() => expect(client.getQueryCache().getAll()[0]?.state.status).toBe("success"));
  expect(container.textContent).toBe("");
});
