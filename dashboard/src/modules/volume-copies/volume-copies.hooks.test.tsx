// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as runs from "#/modules/volume-run/volume-run.functions";
import * as functions from "./volume-copies.functions";
import { useRefreshMirror } from "./volume-copies.hooks";

afterEach(() => vi.restoreAllMocks());

const environment = { project: "shop", environment: "production" };

it("Refresh stops waiting once its run is lost, and says so", async () => {
  vi.spyOn(functions, "refreshMirrorServerFn").mockResolvedValue({ ok: true, run: { id: "run-1" } } as never);
  vi.spyOn(functions, "lastMirrorRefreshServerFn").mockResolvedValue({ ended: null, done: null });
  const read = vi.spyOn(runs, "getVolumeRunServerFn").mockResolvedValue({ id: "run-1", state: "lost", message: null } as never);
  const error = vi.spyOn(toast, "error").mockReturnValue("");
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useRefreshMirror("acme", environment, "v1"), { wrapper });

  result.current.refresh();
  await waitFor(() => expect(error).toHaveBeenCalledWith("Refresh failed"));
  expect(result.current.pending).toBe(false);
  const reads = read.mock.calls.length;
  await new Promise((resolve) => setTimeout(resolve, 3_500));
  expect(read).toHaveBeenCalledTimes(reads);
});
