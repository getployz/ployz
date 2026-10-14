// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as functions from "./volume-copies.functions";
import { useRefreshMirror } from "./volume-copies.hooks";

afterEach(() => vi.restoreAllMocks());

const environment = { project: "shop", environment: "production" };

it("Refresh says what the sync did and stops pending once the mirror has the pull", async () => {
  vi.spyOn(functions, "refreshMirrorServerFn").mockResolvedValue({ ok: true, lines: ["data-web-2 is pulling w-1-5"] } as never);
  vi.spyOn(functions, "mirrorStatusesServerFn").mockResolvedValue([]);
  const success = vi.spyOn(toast, "success").mockReturnValue("");
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useRefreshMirror("acme", environment, "v1"), { wrapper });

  result.current.refresh();
  await waitFor(() => expect(success).toHaveBeenCalledWith("data-web-2 is pulling w-1-5"));
  await waitFor(() => expect(result.current.pending).toBe(false));
});
