// @vitest-environment jsdom
import type { MetricMarker } from "@ployz/sdk";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { MetricRead } from "#/modules/metrics/metrics";
import * as functions from "#/modules/metrics/metrics.functions";
import { serviceMarkersQueryOptions } from "#/modules/metrics/metrics.queries";
import { containerId, machineId, serviceId } from "#/test/ids";
import { useContainerTrouble } from "./use-container-trouble";

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const MINUTE = 60_000;
const scope = { organizationSlug: "acme", environmentId: "env" };
const machine = machineId("a");

const markerAt = (kind: MetricMarker["kind"], atMs: number): MetricMarker => ({
  machine_id: machine, container_id: containerId("c"), service_id: serviceId("s"),
  service: "ns/web", replica: "web-1", ts: atMs * 1e6, kind,
});
const oomAt = (atMs: number) => markerAt("oom", atMs);
const readWith = (first: MetricMarker, ...rest: MetricMarker[]): MetricRead => ({
  type: "history",
  window: { from: first.ts / 1e6 - MINUTE, to: (rest.at(-1) ?? first).ts / 1e6 + MINUTE },
  at: (rest.at(-1) ?? first).ts / 1e6,
  history: {
    step_nanos: 60 * MINUTE * 1e6, points: { service: [] }, unanswered: [],
    machines: [{ machine_id: machine, value: { points: { service: [] }, step_nanos: 60 * MINUTE * 1e6, gaps: [], markers: [first, ...rest] } }],
  },
});

function render(read: MetricRead, updatedAt: number) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  client.setQueryData(serviceMarkersQueryOptions(scope, "web").queryKey, read, { updatedAt });
  const failed = vi.spyOn(functions, "readServiceMetricsServerFn").mockRejectedValue(new Error("unavailable"));
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>;
  return { failed, ...renderHook(() => useContainerTrouble(scope, "web"), { wrapper }) };
}

it("drops a cached OOM from over an hour ago when the read that would replace it fails", async () => {
  const now = Date.now();
  const { result, failed } = render(readWith(oomAt(now - 150 * MINUTE)), now - 150 * MINUTE);
  await waitFor(() => expect(failed).toHaveBeenCalled());
  expect(result.current).toBeNull();
});

it("shows a fresh OOM, then drops it an hour after it happened without another read", () => {
  vi.useFakeTimers({ toFake: ["Date", "setTimeout", "clearTimeout"] });
  const now = Date.now();
  const { result } = render(readWith(oomAt(now - 30 * MINUTE)), now);
  expect(result.current).toBe("OOM");
  act(() => vi.advanceTimersByTime(29 * MINUTE));
  expect(result.current).toBe("OOM");
  act(() => vi.advanceTimersByTime(2 * MINUTE));
  expect(result.current).toBeNull();
});

it("drops Restarting ten minutes after a crash loop's latest restart without another read", () => {
  vi.useFakeTimers({ toFake: ["Date", "setTimeout", "clearTimeout"] });
  const now = Date.now();
  const crashed = (ago: number) => [{ ...markerAt("exit", now - ago * MINUTE), exit_code: 1 }, markerAt("restart", now - ago * MINUTE + 1_000)];
  const { result } = render(readWith(markerAt("restart", now - 20 * MINUTE), ...crashed(8), ...crashed(6)), now);
  expect(result.current).toBe("Restarting");
  const minutes = (count: number) => {
    for (let minute = 0; minute < count; minute++) act(() => vi.advanceTimersByTime(MINUTE));
  };
  minutes(3);
  expect(result.current).toBe("Restarting");
  minutes(2);
  expect(result.current).toBeNull();
});
