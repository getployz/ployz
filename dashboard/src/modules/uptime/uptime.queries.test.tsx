// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { toast } from "sonner";
import { afterEach, expect, it, vi } from "vitest";
import type { ServiceUptime } from "./uptime-status";
import * as functions from "./uptime-status.functions";
import { useRequestUptimeProbe, useUnreachableHost } from "./uptime.queries";
import { organizationKeys } from "#/modules/organization/organization-state.queries";

afterEach(() => vi.restoreAllMocks());

it("toasts Cloud's words when Check now is refused, and a fallback when it sent none", async () => {
  const error = vi.spyOn(toast, "error").mockImplementation(() => "toast");
  vi.spyOn(functions, "requestUptimeProbeServerFn")
    .mockRejectedValueOnce({ _tag: "PublicError", code: "NOT_FOUND", message: "No uptime check." })
    .mockRejectedValueOnce({ reason: "offline" });
  const queryClient = new QueryClient();
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useRequestUptimeProbe({ organizationSlug: "acme", environmentId: "env" }, "web"), { wrapper });

  act(() => result.current.mutate());
  await waitFor(() => expect(error).toHaveBeenCalledWith("No uptime check."));
  act(() => result.current.mutate());
  await waitFor(() => expect(error).toHaveBeenLastCalledWith("Could not check now"));
});

it("reads the Service's probe as pending before Cloud answers, and puts it back when Cloud refuses", async () => {
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  let refuse: (error: Error) => void = () => undefined;
  vi.spyOn(functions, "requestUptimeProbeServerFn").mockReturnValue(new Promise((_, reject) => { refuse = reject; }));
  const queryClient = new QueryClient({ defaultOptions: { queries: { staleTime: Infinity } } });
  const uptime: ServiceUptime = {
    check: { id: "check", url: "https://web.example", intervalSeconds: 60 },
    status: "up", uptime: 1, points: [], checks: [], latest: [], incidents: [], regions: {}, probe: null,
  };
  const day = ["uptime", "env", "web", "-24h", "now"];
  const other = ["uptime", "env", "api", "-24h", "now"];
  queryClient.setQueryData(day, uptime);
  queryClient.setQueryData(other, uptime);
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useRequestUptimeProbe({ organizationSlug: "acme", environmentId: "env" }, "web"), { wrapper });

  act(() => result.current.mutate());
  await waitFor(() => expect(queryClient.getQueryData<ServiceUptime>(day)?.probe).toMatchObject({ answered: false }));
  expect(queryClient.getQueryData<ServiceUptime>(other)?.probe).toBeNull();

  act(() => refuse(new Error("offline")));
  await waitFor(() => expect(queryClient.getQueryData<ServiceUptime>(day)?.probe).toBeNull());
});

it.each([[true, "api.acme.dev"], [false, null]])("reads a Service's open incident for the canvas only where the uptime checker runs (%s)", async (uptimeEnabled, host) => {
  const read = vi.spyOn(functions, "readEnvironmentUptimeServerFn")
    .mockResolvedValue([{ serviceId: "api", url: "https://api.acme.dev/health", openIncidentAt: "2026-10-12T00:00:00Z" }]);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  queryClient.setQueryData(organizationKeys.state("acme"), { uptimeEnabled });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const { result } = renderHook(() => useUnreachableHost({ organizationSlug: "acme", environmentId: "env" }, "api"), { wrapper });

  await waitFor(() => expect(result.current).toBe(host));
  await new Promise((resolve) => setTimeout(resolve, 20));
  expect(result.current).toBe(host);
  expect(read).toHaveBeenCalledTimes(uptimeEnabled ? 1 : 0);
});
