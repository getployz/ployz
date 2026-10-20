// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ServerStatus } from "#/modules/machines/server-status";
import type { StoredReport } from "#/modules/machines/waiting/checkin-wire";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import * as waitingFunctions from "#/modules/machines/waiting/waiting.functions";
import { EnrollSheet } from "./enroll-sheet";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const report: StoredReport = {
  hostname: "web", os: { id: "ubuntu", version: "24.04" }, kernel: "6.8.0", arch: "x86_64", virt: "kvm", cpuCount: 4,
  memoryTotalBytes: 8 * 1024 ** 3, disks: [{ path: "/dev/sda", sizeBytes: 80 * 1024 ** 3, role: "root" }],
  docker: { kind: "absent" }, cgroup: "v2", zfs: { kind: "eligible", targets: [{ kind: "main_disk" }] }, blockers: [], warnings: [],
};
const waiting: WaitingServerView = {
  id: "id-web", report, observedIp: "203.0.113.7", fleetToken: "brisk-otter", status: "waiting",
  firstSeenAt: "2026-10-17T09:59:00Z", lastCheckInAt: "2026-10-17T09:59:55Z", arrivalAt: "2026-10-17T09:59:00Z", returned: null,
};

async function open(fleet: readonly { status: ServerStatus }[]) {
  vi.spyOn(waitingFunctions, "planServerEnrollmentServerFn").mockImplementation(async ({ data }) => ({
    rows: data.ids.map((id) => ({ waitingServerId: id, fleetTokenId: "fleet", hostname: "web", role: "joiner" as const,
      instruction: { name: "web", storage: { kind: "zfs" as const }, policy: { labels: {}, acceptsBuilds: true, acceptsServices: true, acceptsIngress: true } } })),
    skipped: [], spread: { providers: 1, networks: 1 },
  }));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createRouter({
    routeTree: createRootRoute({ component: () => <QueryClientProvider client={client}>
      <EnrollSheet organizationSlug="ada" servers={[waiting]} hasFirst fleet={fleet} onClose={vi.fn()} />
    </QueryClientProvider> }),
    history: createMemoryHistory(),
  });
  await router.load();
  render(<RouterProvider router={router} />);
  await vi.waitFor(() => expect(screen.getByText("ZFS on main disk")).toBeTruthy());
}

it("warns when every Server is offline, links to Forget Servers, and still lets you enroll", async () => {
  await open([{ status: "offline" }, { status: "offline" }]);
  expect(screen.getByRole("alert").textContent).toBe("Servers offlineForget them first. Forget Servers");
  expect(screen.getByRole("link", { name: "Forget Servers" }).getAttribute("href")).toBe("/cloud/ada/~/settings#organization-teardown");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Enroll 1" }).disabled).toBe(false);
});

it("doesn't warn while any Server is online", async () => {
  await open([{ status: "offline" }, { status: "online" }]);
  expect(screen.queryByText("Servers offline")).toBeNull();
});
