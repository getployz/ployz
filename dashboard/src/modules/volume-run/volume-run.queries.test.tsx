// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import type { VolumeRunView } from "./volume-run";
import * as functions from "./volume-run.functions";
import { useWatchVolumeRun } from "./volume-run.queries";

afterEach(() => vi.restoreAllMocks());

const done: VolumeRunView = {
  id: "run-1", volume_id: "vol", volume_name: "data", kind: "sync", args: {}, orphan: false,
  state: "done", epoch: null, message: null, created_at: "", updated_at: "", finished_at: "",
};

function watch() {
  const onEnd = vi.fn();
  const queryClient = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useWatchVolumeRun("acme", onEnd), { wrapper });
  act(() => result.current.watch("run-1"));
  return onEnd;
}

it("keeps watching a run through one failed read, and hands over the run when it ends", async () => {
  vi.spyOn(functions, "getVolumeRunServerFn").mockRejectedValueOnce(new Error("offline")).mockResolvedValue(done);
  const onEnd = watch();
  await waitFor(() => expect(onEnd).toHaveBeenCalled(), { timeout: 4_000 });
  expect(onEnd).toHaveBeenCalledExactlyOnceWith(done);
});

it("stops watching a run it still cannot read after retrying", async () => {
  vi.spyOn(functions, "getVolumeRunServerFn").mockRejectedValue(new Error("offline"));
  const onEnd = watch();
  await waitFor(() => expect(onEnd).toHaveBeenCalled(), { timeout: 4_000 });
  expect(onEnd).toHaveBeenCalledExactlyOnceWith(null);
});
