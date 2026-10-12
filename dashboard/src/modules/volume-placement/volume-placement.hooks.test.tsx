// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as runs from "#/modules/volume-run/volume-run.functions";
import * as functions from "./volume-placement.functions";
import { useStagedPlacements } from "./volume-placement.hooks";
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
