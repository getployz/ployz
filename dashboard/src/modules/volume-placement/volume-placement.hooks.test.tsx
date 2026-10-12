// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as runs from "#/modules/volume-run/volume-run.functions";
import * as functions from "./volume-placement.functions";
import { useDeleteCopy, useStagedPlacements } from "./volume-placement.hooks";
import type { PlacementRow } from "./volume-placement";

afterEach(() => vi.restoreAllMocks());

const environment = { project: "shop", environment: "production" };

it("Discard all discards a staged restore the Servers haven't answered for yet, once the Store discard lands", async () => {
  const restore: PlacementRow = {
    volumeId: "vol-restored", environmentId: "env-1", mirrors: [], deploymentId: null,
    staged: {
      primary: "web-1".padEnd(32, "0") as never, mirrors: [], deletes: [],
      seed: { sourceVolumeId: "vol-source", sourceName: "data", dockerVolume: "shop_vol-restored", point: { guid: "1", name: "p", at: 1 } },
    },
  };
  vi.spyOn(functions, "listPlacementsServerFn").mockResolvedValue([restore] as never);
  vi.spyOn(runs, "inspectVolumeCopiesServerFn").mockReturnValue(new Promise(() => undefined) as never);
  const discarded = vi.spyOn(functions, "discardPlacementServerFn").mockResolvedValue({ ok: true, placement: null } as never);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  // The Store discard already took the restore's Volume, so the canvas no longer lists it.
  const { result } = renderHook(() => useStagedPlacements("acme", environment, "env-1", []), { wrapper });
  await waitFor(() => expect(queryClient.getQueryData(["volume-placement", "acme"])).toEqual([restore]));

  let storeDiscarded = false;
  result.current.discardAll(async () => { storeDiscarded = true; return undefined; });

  await waitFor(() => expect(discarded).toHaveBeenCalledWith({ data: { organizationSlug: "acme", volumeId: "vol-restored", environment } }));
  expect(storeDiscarded).toBe(true);
  expect(result.current.groups).toEqual([]);
});

const runView = (id: string, state: string, message: string | null = null) => ({ id, state, message }) as never;

it("Delete copy toasts only once the delete run ends, with the Server's words when it fails", async () => {
  vi.spyOn(functions, "deleteCopyServerFn").mockResolvedValueOnce({ ok: true, run: { id: "run-1" } } as never)
    .mockResolvedValueOnce({ ok: true, run: { id: "run-2" } } as never);
  const read = vi.spyOn(runs, "getVolumeRunServerFn").mockResolvedValueOnce(runView("run-1", "running"))
    .mockResolvedValueOnce(runView("run-1", "done")).mockResolvedValue(runView("run-2", "failed", "web-3 still mounts data."));
  const success = vi.spyOn(toast, "success").mockReturnValue("");
  const error = vi.spyOn(toast, "error").mockReturnValue("");
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useDeleteCopy("acme", environment, { id: "v1", name: "data" }), { wrapper });

  await result.current("web-3");
  await waitFor(() => expect(read).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "run-1" } }));
  expect(success).not.toHaveBeenCalled();
  await waitFor(() => expect(success).toHaveBeenCalledWith("Copy deleted"), { timeout: 5_000 });

  await result.current("web-3");
  await waitFor(() => expect(error).toHaveBeenCalledWith("web-3 still mounts data."));
  expect(success).toHaveBeenCalledTimes(1);
});

it("Delete copy toasts a refusal without watching anything", async () => {
  vi.spyOn(functions, "deleteCopyServerFn").mockResolvedValue({ ok: false, refusal: { message: "data is busy." } } as never);
  const read = vi.spyOn(runs, "getVolumeRunServerFn");
  const error = vi.spyOn(toast, "error").mockReturnValue("");
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useDeleteCopy("acme", environment, { id: "v1", name: "data" }), { wrapper });

  await result.current("web-3");
  expect(error).toHaveBeenCalledWith("data is busy.");
  expect(read).not.toHaveBeenCalled();
});
