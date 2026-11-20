// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { CrashMarker, MetricRead, ServiceHistory } from "#/modules/metrics/metrics";
import * as functions from "#/modules/metrics/metrics.functions";
import { serviceMarkersQueryOptions } from "#/modules/metrics/metrics.queries";
import { machineId } from "#/test/ids";
import { useContainerTrouble } from "./use-container-trouble";

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const MINUTE = 60_000;
const scope = { organizationSlug: "acme", environmentId: "env" };
const machine = machineId("a");

const oomAt = (at: number): CrashMarker => ({ kind: "oom", at, machine, replica: "web-1" });
const readWith = (marker: CrashMarker): MetricRead<ServiceHistory> => ({
  type: "history",
  window: { from: marker.at - MINUTE, to: marker.at + MINUTE },
  at: marker.at,
  history: { stepMs: 60 * MINUTE, points: [], servers: [{ machine, points: [], gaps: [], markers: [marker] }] },
});

function render(read: MetricRead<ServiceHistory>, updatedAt: number) {
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
