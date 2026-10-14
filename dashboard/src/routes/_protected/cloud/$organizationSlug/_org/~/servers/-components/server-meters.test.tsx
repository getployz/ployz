// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { serverStatus } from "#/modules/machines/server-status";
import * as functions from "#/modules/metrics/metrics.functions";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { ServerMeters } from "./server-meters";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

const readServerMetrics = () => vi.spyOn(functions, "readServerMetricsServerFn").mockReturnValue(new Promise(() => {}) as never);

function viewport({ wide }: { wide: boolean }) {
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: wide, media: query, addEventListener: () => {}, removeEventListener: () => {} }));
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
