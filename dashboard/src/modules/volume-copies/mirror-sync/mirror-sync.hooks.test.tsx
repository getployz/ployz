// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import * as functions from "./mirror-sync.functions";
import { useRefreshMirror } from "./mirror-sync.hooks";

afterEach(() => vi.restoreAllMocks());

const environment = { project: "shop", environment: "production" };

it("Refresh stops pending once the mirror has the pull", async () => {
  let answer!: () => void;
  vi.spyOn(functions, "refreshMirrorServerFn").mockReturnValue(
    new Promise((resolve) => { answer = () => resolve({ ok: true, lines: ["data-web-2 is pulling w-1-5"] }); }) as never,
  );
  vi.spyOn(functions, "mirrorStatusesServerFn").mockResolvedValue([]);
  vi.spyOn(toast, "success").mockReturnValue("");
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useRefreshMirror("acme", environment, "v1"), { wrapper });

  act(() => result.current.refresh());
  await waitFor(() => expect(result.current.pending).toBe(true));
  answer();
  await waitFor(() => expect(result.current.pending).toBe(false));
});
