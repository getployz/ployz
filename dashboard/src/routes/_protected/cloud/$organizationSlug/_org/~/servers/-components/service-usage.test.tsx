// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen } from "@testing-library/react";
import type { MachineId } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as functions from "#/modules/metrics/metrics.functions";
import type { MetricRead } from "#/modules/metrics/metrics";
import { runtimeServiceMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import { NOW_RANGE } from "./server-now";
import { ServiceUsage } from "./service-usage";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const SYD_1 = "1".repeat(32) as MachineId;
const HOUR = 3_600_000;
const nanos = (ms: number) => ms * 1e6;
const options = runtimeServiceMetricsQueryOptions({ organizationSlug: "acme", namespace: "shop", name: "api", ...NOW_RANGE });

function readAt(to: number): MetricRead {
  const points = { service: [{ ts: nanos(to - 10_000), cpu_usec: 370_000, mem_bytes: 2 * 1024 * 1024 }] };
  return {
    type: "history",
    window: { from: to - 300_000, to },
    at: to,
    history: { step_nanos: nanos(10_000), points, machines: [{ machine_id: SYD_1, value: { points, step_nanos: nanos(10_000), gaps: [] } }], unanswered: [] },
  };
}

function show(read: MetricRead, landedAt: number) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(options.queryKey, read, { updatedAt: landedAt });
  render(
    <QueryClientProvider client={client}>
      <ServiceUsage organizationSlug="acme" server={{ id: SYD_1, name: "syd-1" }} namespace="shop" name="api" />
    </QueryClientProvider>,
  );
  return client;
}

it("drops the Service's usage when a refresh fails", async () => {
  const reads = vi.spyOn(functions, "readRuntimeServiceMetricsServerFn").mockRejectedValue(new Error("unreachable"));
  const client = show(readAt(Date.now()), Date.now());
  expect(screen.getByText("37% CPU · 2 MB")).toBeTruthy();

  await act(() => client.refetchQueries({ queryKey: options.queryKey }));
  expect(reads).toHaveBeenCalled();
  expect(await screen.findByText("–")).toBeTruthy();
  expect(screen.queryByText(/CPU/)).toBeNull();
});

it("doesn't call an hour-old cached read current while its refresh is out", () => {
  vi.spyOn(functions, "readRuntimeServiceMetricsServerFn").mockReturnValue(new Promise(() => {}) as never);
  show(readAt(Date.now() - HOUR), Date.now() - HOUR);
  expect(screen.getByText("–")).toBeTruthy();
  expect(screen.queryByText(/CPU/)).toBeNull();
});
