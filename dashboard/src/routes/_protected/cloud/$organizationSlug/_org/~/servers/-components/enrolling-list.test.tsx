// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ServerEnrollmentView } from "#/modules/machines/waiting/enroll.server";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { EnrollmentCounters, EnrollingList, waitingAfterReturns } from "./enrolling-list";

afterEach(cleanup);
const attempt = (n: number, patch: Partial<ServerEnrollmentView> = {}): ServerEnrollmentView => ({
  id: `attempt-${n}`, waitingServerId: `host-${n}`, role: "joiner", name: `web-${n}`, storage: { kind: "none" }, state: "running", stage: "installing", queued: false, ahead: null, machineId: null, failure: null, requestedAt: `2026-10-17T10:00:${String(n).padStart(2, "0")}Z`, endedAt: null, ...patch,
});
const host = (n: number): WaitingServerView => ({
  id: `host-${n}`, report: { hostname: `web-${n}`, os: { id: "ubuntu", version: "24.04" }, kernel: "6.8", arch: "x86_64", virt: "kvm", cpuCount: 2, memoryTotalBytes: 4 * 1024 ** 3, disks: [], docker: { kind: "absent" }, cgroup: "v2", zfs: { kind: "eligible", targets: [] }, blockers: [], warnings: [] },
  observedIp: "203.0.113.1", fleetToken: "test", status: "waiting", firstSeenAt: `2026-10-17T09:00:${String(n).padStart(2, "0")}Z`, lastCheckInAt: "2026-10-17T10:00:00Z", probe: null,
});
const rows = () => within(screen.getByRole("table", { name: "Enrolling servers" })).getAllByRole("row").slice(1);

it("appends arrivals in order and retains Joined rows and their DOM position across polls", () => {
  const onCancel = vi.fn();
  const first = [attempt(1), attempt(2)];
  const view = render(<EnrollingList joining={0} attempts={first} onCancel={onCancel} />);
  const before = rows();
  view.rerender(<EnrollingList joining={0} attempts={[attempt(1, { state: "joined", endedAt: "2026-10-17T10:01:00Z" }), attempt(2, { stage: "joining" }), attempt(3)]} onCancel={onCancel} />);
  expect(rows().map((row) => within(row).getAllByRole("cell")[0]?.textContent)).toEqual(["web-1", "web-2", "web-3"]);
  expect(rows().slice(0, 2)).toEqual(before);
  expect(within(rows()[0] ?? expect.unreachable("first row")).getByText("Joined")).toBeTruthy();
});

it("retains the entire list when every server has joined", () => {
  render(<EnrollingList joining={0} attempts={[attempt(1, { state: "joined", endedAt: "2026-10-17T10:01:00Z" })]} onCancel={vi.fn()} />);
  expect(rows()).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Cancel web-1" })).toBeNull();
});

it("draws all six segments for Docker-only, ZFS, queued and Joined rows", () => {
  render(<EnrollingList joining={0} attempts={[attempt(1), attempt(2, { storage: { kind: "zfs" } }), attempt(3, { queued: true, state: "instructed" }), attempt(4, { state: "joined", endedAt: "2026-10-17T10:01:00Z" })]} onCancel={vi.fn()} />);
  for (const row of rows()) expect(row.querySelectorAll("[data-segment]")).toHaveLength(6);
});

it("keeps preparation Queued distinct from In line, including an unknown join position", () => {
  render(<EnrollingList joining={0} attempts={[attempt(1, { queued: true, state: "instructed", stage: null }), attempt(2, { stage: "queued", ahead: 3 }), attempt(3, { stage: "queued" })]} onCancel={vi.fn()} />);
  expect(screen.getByText("Queued")).toBeTruthy();
  expect(screen.getByText("3 servers ahead")).toBeTruthy();
  expect(screen.getByText("Position unknown")).toBeTruthy();
  const stages = screen.getByRole("group", { name: "Enrollment stages" });
  expect(stages.querySelector('[data-counter="Starting"]')?.textContent).toBe("Starting1");
  expect(stages.querySelector('[data-counter="In line"]')?.textContent).toBe("In line2");
});

it("moves failed or cancelled hosts to the bottom of Waiting in return order", () => {
  const attempts = [attempt(2, { state: "failed", endedAt: "2026-10-17T10:02:00Z" }), attempt(1, { state: "cancelled", endedAt: "2026-10-17T10:01:00Z" })];
  const result = waitingAfterReturns([host(1), host(2), host(3), host(4)], attempts);
  expect(result.rows.map((row) => row.id)).toEqual(["host-3", "host-4", "host-1", "host-2"]);
  render(<EnrollingList joining={0} attempts={attempts} onCancel={vi.fn()} />);
  expect(screen.queryByRole("table")).toBeNull();
});

it("keeps all six fleet and stage counters at fixed equal widths, including zeros", () => {
  const view = render(<><EnrollmentCounters joining={0} waiting={[]} attempts={[]} /><EnrollingList joining={0} attempts={[attempt(1)]} onCancel={vi.fn()} /></>);
  const grids = screen.getAllByRole("group");
  expect(grids).toHaveLength(2);
  for (const grid of grids) {
    expect(grid.classList.contains("grid-cols-6")).toBe(true);
    expect(grid.querySelectorAll("[data-counter]")).toHaveLength(6);
    for (const value of grid.querySelectorAll("dd")) expect(value.className).toContain("w-14 shrink-0");
  }
  const before = [...(grids[0] ?? expect.unreachable("totals")).querySelectorAll("dd")].map((value) => value.className);
  view.rerender(<EnrollmentCounters joining={0} waiting={Array.from({ length: 200 }, (_, n) => host(n))} attempts={Array.from({ length: 50 }, (_, n) => attempt(n))} />);
  expect([...screen.getByRole("group").querySelectorAll("dd")].map((value) => value.className)).toEqual(before);
});

it("cancels the named attempt and holds every row at 52 pixels inside a ten-row box", () => {
  const onCancel = vi.fn();
  render(<EnrollingList joining={0} attempts={Array.from({ length: 12 }, (_, n) => attempt(n + 1))} onCancel={onCancel} />);
  fireEvent.click(screen.getByRole("button", { name: "Cancel web-2" }));
  expect(onCancel).toHaveBeenCalledWith("attempt-2");
  expect(document.querySelector<HTMLElement>('[data-slot="enrolling-box"]')?.style.maxHeight).toBe("520px");
  expect(new Set(rows().map((row) => row.style.height))).toEqual(new Set(["52px"]));
});

it("appends a new arrival after hosts already returned to Waiting", () => {
  const returned = attempt(1, { state: "failed", endedAt: "2026-10-17T10:01:00Z" });
  const later = { ...host(3), firstSeenAt: "2026-10-17T10:02:00Z" };
  expect(waitingAfterReturns([later, host(1), host(2)], [returned]).rows.map((row) => row.id)).toEqual(["host-2", "host-1", "host-3"]);
});

it("counts Cloud slot occupancy while completed workflows still report joining", () => {
  const attempts = Array.from({ length: 9 }, (_, n) => attempt(n, { stage: "joining" }));
  render(<><EnrollmentCounters waiting={[]} attempts={attempts} joining={8} /><EnrollingList attempts={attempts} joining={8} onCancel={vi.fn()} /></>);
  expect(screen.getAllByText("8 / 8")).toHaveLength(2);
  expect(screen.queryByText("9 / 8")).toBeNull();
});
