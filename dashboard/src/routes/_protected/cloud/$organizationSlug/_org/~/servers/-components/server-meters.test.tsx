// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { serverStatus } from "#/modules/machines/server-status";
import * as functions from "#/modules/metrics/metrics.functions";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { ServerMeters } from "./server-meters";

afterEach(() => {
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
  render(
    <QueryClientProvider client={client}>
      <ServerMeters organizationSlug="acme" server={{ machine, name: machine.name, status: serverStatus(machine) }} />
    </QueryClientProvider>,
  );
}

it("says an offline Server didn't answer without dialing it", () => {
  const reads = readServerMetrics();
  viewport({ wide: true });
  showRow("down");
  expect(screen.getByText("Didn’t answer")).toBeTruthy();
  expect(reads).not.toHaveBeenCalled();
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
