// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as runs from "#/modules/volume-run/volume-run.functions";
import { member } from "#/modules/volume-run/volume-run.test-fixture";
import * as functions from "./volume-placement.functions";
import { useDeleteCopy, useStagedPlacements } from "./volume-placement.hooks";
import type { PlacementRow } from "./volume-placement";

afterEach(() => vi.restoreAllMocks());

const environment = { project: "shop", environment: "production" };

it("Discard all discards a staged restore the Servers haven't answered for yet, once the Store discard lands", async () => {
  const restore: PlacementRow = {
    volumeId: "vol-restored", environmentId: "env-1", mirrors: [], unreached: false, deploymentId: null,
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

it("Discard all discards one restore at a time, so no sibling finds the Environment held and stays staged", async () => {
  const restore = (volumeId: string): PlacementRow => ({
    volumeId, environmentId: "env-1", mirrors: [], unreached: false, deploymentId: null,
    staged: {
      primary: "web-1".padEnd(32, "0") as never, mirrors: [], deletes: [],
      seed: { sourceVolumeId: "vol-source", sourceName: "data", dockerVolume: `shop_${volumeId}`, point: { guid: "1", name: "p", at: 1 } },
    },
  });
  vi.spyOn(functions, "listPlacementsServerFn").mockResolvedValue([restore("vol-a"), restore("vol-b")] as never);
  vi.spyOn(runs, "inspectVolumeCopiesServerFn").mockReturnValue(new Promise(() => undefined) as never);
  const ends: (() => void)[] = [];
  const discarded = vi.spyOn(functions, "discardPlacementServerFn")
    .mockImplementation(() => new Promise((resolve) => ends.push(() => resolve({ ok: true, placement: null }))) as never);
  const error = vi.spyOn(toast, "error").mockReturnValue("");
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useStagedPlacements("acme", environment, "env-1", []), { wrapper });
  await waitFor(() => expect(queryClient.getQueryData(["volume-placement", "acme"])).toHaveLength(2));

  act(() => result.current.discardAll(async () => undefined));

  await waitFor(() => expect(discarded).toHaveBeenCalledTimes(1));
  await new Promise((resolve) => setTimeout(resolve, 0));
  expect(discarded).toHaveBeenCalledTimes(1);
  act(() => ends[0]?.());
  await waitFor(() => expect(discarded).toHaveBeenCalledTimes(2));
  expect(discarded.mock.calls.map(([call]) => (call as { data: { volumeId: string } }).data.volumeId)).toEqual(["vol-a", "vol-b"]);
  act(() => ends[1]?.());
  await waitFor(() => expect(result.current.count).toBe(0));
  expect(error).not.toHaveBeenCalled();
});

it("leaves a Volume the Deploy deletes out of the copy loss, so only the Store's own deletion confirm accepts it", async () => {
  const id = (server: string) => server.padEnd(32, "0") as never;
  const dropsMirror: PlacementRow = {
    volumeId: "v1", environmentId: "env-1", mirrors: [id("web-2")], unreached: false, deploymentId: null,
    staged: { primary: id("web-1"), mirrors: [], deletes: [] },
  };
  vi.spyOn(functions, "listPlacementsServerFn").mockResolvedValue([dropsMirror] as never);
  vi.spyOn(runs, "inspectVolumeCopiesServerFn").mockResolvedValue([member("web-1", "writer"), member("web-2", "mirror")] as never);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result, rerender } = renderHook((change: "delete" | null) =>
    useStagedPlacements("acme", environment, "env-1", [{ id: "v1", name: "pg-data", change }]), { wrapper, initialProps: null as "delete" | null });
  await waitFor(() => expect(result.current.loss.accept).toEqual(["pg-data"]));

  rerender("delete");
  expect(result.current.loss.accept).toEqual([]);
  expect(result.current.loss.items).toEqual([]);
  expect(result.current.count).toBe(1);
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

  act(() => result.current("web-3"));
  await waitFor(() => expect(read).toHaveBeenCalledWith({ data: { organizationSlug: "acme", id: "run-1" } }));
  expect(success).not.toHaveBeenCalled();
  await waitFor(() => expect(success).toHaveBeenCalledWith("Copy deleted"), { timeout: 5_000 });

  act(() => result.current("web-3"));
  await waitFor(() => expect(error).toHaveBeenCalledWith("web-3 still mounts data."));
  expect(success).toHaveBeenCalledTimes(1);
});

it("Delete copy watches each delete it starts, so a second one leaves the first its toast", async () => {
  vi.spyOn(functions, "deleteCopyServerFn").mockResolvedValueOnce({ ok: true, run: { id: "run-1" } } as never)
    .mockResolvedValueOnce({ ok: true, run: { id: "run-2" } } as never);
  let ended = false;
  vi.spyOn(runs, "getVolumeRunServerFn").mockImplementation(({ data }) => Promise.resolve(runView(data.id, ended ? "done" : "running")));
  const success = vi.spyOn(toast, "success").mockReturnValue("");
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const invalidated = vi.spyOn(queryClient, "invalidateQueries");
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useDeleteCopy("acme", environment, { id: "v1", name: "data" }), { wrapper });

  act(() => result.current("web-3"));
  act(() => result.current("web-4"));
  await waitFor(() => expect(functions.deleteCopyServerFn).toHaveBeenCalledTimes(2));
  ended = true;
  await waitFor(() => expect(success).toHaveBeenCalledTimes(2), { timeout: 5_000 });
  expect(invalidated.mock.calls.filter(([filters]) => JSON.stringify(filters?.queryKey) === JSON.stringify(["volume-placement"]))).toHaveLength(2);
}, 10_000);

it("Delete copy toasts a refusal without watching anything", async () => {
  vi.spyOn(functions, "deleteCopyServerFn").mockResolvedValue({ ok: false, refusal: { message: "data is busy." } } as never);
  const read = vi.spyOn(runs, "getVolumeRunServerFn");
  const error = vi.spyOn(toast, "error").mockReturnValue("");
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={new QueryClient()}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useDeleteCopy("acme", environment, { id: "v1", name: "data" }), { wrapper });

  act(() => result.current("web-3"));
  await waitFor(() => expect(error).toHaveBeenCalledWith("data is busy."));
  expect(read).not.toHaveBeenCalled();
});
