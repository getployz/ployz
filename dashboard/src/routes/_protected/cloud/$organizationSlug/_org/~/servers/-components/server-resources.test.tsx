// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import type { MachineId } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as functions from "#/modules/metrics/metrics.functions";
import type { MetricRead } from "#/modules/metrics/metrics";
import { serverMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import { ServerResources } from "./server-resources";
import { NOW_RANGE } from "./server-now";

beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const SYD_1 = "1".repeat(32) as MachineId;
const HOUR = 3_600_000;
const KB = 1024;
const nanos = (ms: number) => ms * 1e6;
const history = serverMetricsQueryOptions({ organizationSlug: "acme", machineId: SYD_1, from: "1h", to: "now" });
const short = serverMetricsQueryOptions({ organizationSlug: "acme", machineId: SYD_1, ...NOW_RANGE });

function readAt(to: number, from: number): MetricRead {
  const points = { machine: [{
    ts: nanos(to - 10_000),
    cpu_busy: 0.37,
    memory: { total: 64 * KB * KB, available: 48 * KB * KB },
    interfaces: { eth0: { rx: KB, tx: 2 * KB }, eth1: { rx: 3 * KB, tx: 4 * KB } },
    disk: { total: 100 * KB * KB, free: 77 * KB * KB },
  }] };
  return {
    type: "history",
    window: { from, to },
    at: to,
    history: { step_nanos: nanos(10_000), points, machines: [{ machine_id: SYD_1, value: { points, step_nanos: nanos(10_000), gaps: [] } }], unanswered: [] },
  };
}

async function show(landedAt: number) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(history.queryKey, readAt(landedAt, landedAt - HOUR), { updatedAt: landedAt });
  client.setQueryData(short.queryKey, readAt(landedAt, landedAt - 300_000), { updatedAt: landedAt });
  const root = createRootRoute({
    loader: () => ({ timeZone: "UTC" }),
    component: () => <ServerResources organizationSlug="acme" server={{ id: SYD_1, name: "syd-1" }} />,
  });
  const router = createRouter({ routeTree: root, history: createMemoryHistory() });
  render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("heading", { name: "Network" });
  return client;
}

const nowValues = () => ({
  meters: screen.queryAllByRole("meter").map((meter) => meter.getAttribute("aria-label")),
  network: screen.queryByText(/^In .* · Out /)?.textContent ?? null,
  disk: screen.queryAllByText("23 MB / 100 MB").length,
});

it("reads every now-value from the short read, summing the interfaces", async () => {
  vi.spyOn(functions, "readServerMetricsServerFn").mockReturnValue(new Promise(() => {}) as never);
  await show(Date.now());
  expect(nowValues()).toEqual({ meters: ["CPU", "Memory", "Disk"], network: "In 4 KB/s · Out 6 KB/s", disk: 2 });
});

it("drops the now-values but keeps the charts when the short read's refresh fails", async () => {
  vi.spyOn(functions, "readServerMetricsServerFn").mockRejectedValue(new Error("unreachable"));
  const client = await show(Date.now());
  expect(nowValues().meters).toHaveLength(3);

  await act(() => client.refetchQueries({ queryKey: short.queryKey }));
  await waitFor(() => expect(nowValues()).toEqual({ meters: [], network: null, disk: 0 }));
  expect(screen.getByRole("heading", { name: "Network" })).toBeTruthy();
  expect(screen.queryByText(/didn’t answer/)).toBeNull();
});

it("doesn't call an hour-old cached read current while its refresh is out", async () => {
  vi.spyOn(functions, "readServerMetricsServerFn").mockReturnValue(new Promise(() => {}) as never);
  await show(Date.now() - HOUR);
  expect(nowValues()).toEqual({ meters: [], network: null, disk: 0 });
});
