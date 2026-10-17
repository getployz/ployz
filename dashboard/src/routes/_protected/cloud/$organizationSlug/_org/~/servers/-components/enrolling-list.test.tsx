// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ServerEnrollmentView } from "#/modules/machines/waiting/enroll.server";
import type { WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { EnrollmentCounters, EnrollingList } from "./enrolling-list";

afterEach(cleanup);
const attempt = (n: number, patch: Partial<ServerEnrollmentView> = {}): ServerEnrollmentView => ({
  id: `attempt-${n}`, waitingServerId: `host-${n}`, role: "joiner", name: `web-${n}`, storage: { kind: "none" }, state: "running", stage: "installing", queued: false, ahead: null, machineId: null, failure: null, requestedAt: `2026-10-17T10:00:${String(n).padStart(2, "0")}Z`, endedAt: null, ...patch,
});
const host = (n: number): WaitingServerView => ({
  id: `host-${n}`, report: { hostname: `web-${n}`, os: { id: "ubuntu", version: "24.04" }, cpuCount: 2, memoryTotalBytes: 4 * 1024 ** 3, disks: [], zfs: { kind: "eligible" }, blockers: [] },
  observedIp: "203.0.113.1", fleetToken: "test", status: "waiting", firstSeenAt: `2026-10-17T09:00:${String(n).padStart(2, "0")}Z`, lastCheckInAt: "2026-10-17T10:00:00Z", arrivalAt: `2026-10-17T09:00:${String(n).padStart(2, "0")}Z`, returned: null,
});
const rows = () => within(screen.getByRole("table", { name: "Enrolling servers" })).getAllByRole("row").slice(1);

it("appends arrivals in order and retains Joined rows and their DOM position across polls", () => {
  const onCancel = vi.fn();
  const first = [attempt(1), attempt(2)];
  const view = render(<EnrollingList attempts={first} onCancel={onCancel} />);
  const before = rows();
  view.rerender(<EnrollingList attempts={[attempt(1, { state: "joined", endedAt: "2026-10-17T10:01:00Z" }), attempt(2, { stage: "joining" }), attempt(3)]} onCancel={onCancel} />);
  expect(rows().map((row) => within(row).getAllByRole("cell")[0]?.textContent)).toEqual(["web-1", "web-2", "web-3"]);
  expect(rows().slice(0, 2)).toEqual(before);
  expect(within(rows()[0] ?? expect.unreachable("first row")).getByText("Joined")).toBeTruthy();
});

it("retains the entire list when every server has joined", () => {
  render(<EnrollingList attempts={[attempt(1, { state: "joined", endedAt: "2026-10-17T10:01:00Z" })]} onCancel={vi.fn()} />);
  expect(rows()).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Cancel web-1" })).toBeNull();
});

it("draws all six segments for Docker-only, ZFS, queued and Joined rows", () => {
  render(<EnrollingList attempts={[attempt(1), attempt(2, { storage: { kind: "zfs" } }), attempt(3, { queued: true, state: "instructed" }), attempt(4, { state: "joined", endedAt: "2026-10-17T10:01:00Z" })]} onCancel={vi.fn()} />);
  for (const row of rows()) expect(row.querySelectorAll("[data-segment]")).toHaveLength(6);
});

it("keeps preparation Queued distinct from In line, including an unknown join position", () => {
  render(<EnrollingList attempts={[attempt(1, { queued: true, state: "instructed", stage: null }), attempt(2, { stage: "queued", ahead: 3 }), attempt(3, { stage: "queued" })]} onCancel={vi.fn()} />);
  expect(screen.getByText("Queued")).toBeTruthy();
  expect(screen.getByText("3 servers ahead")).toBeTruthy();
  expect(screen.queryByText("Position unknown")).toBeNull();
  expect(screen.queryByText("Waiting for preparation")).toBeNull();
  const stages = screen.getByRole("group", { name: "Enrollment stages" });
  expect(stages.querySelector('[data-counter="Starting"]')?.textContent).toBe("Starting1");
  expect(stages.querySelector('[data-counter="In line"]')?.textContent).toBe("In line2");
});

it("moves failed or cancelled hosts to the bottom of Waiting in return order", () => {
  const attempts = [attempt(2, { state: "failed", endedAt: "2026-10-17T10:02:00Z" }), attempt(1, { state: "cancelled", endedAt: "2026-10-17T10:01:00Z" })];
  render(<EnrollingList attempts={attempts} onCancel={vi.fn()} />);
  expect(screen.queryByRole("table")).toBeNull();
});

it("keeps all six fleet and stage counters at fixed equal widths, including zeros", () => {
  const view = render(<><EnrollmentCounters joining={0} waiting={[]} attempts={[]} /><EnrollingList attempts={[attempt(1)]} onCancel={vi.fn()} /></>);
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
  render(<EnrollingList attempts={Array.from({ length: 12 }, (_, n) => attempt(n + 1))} onCancel={onCancel} />);
  fireEvent.click(screen.getByRole("button", { name: "Cancel web-2" }));
  expect(onCancel).toHaveBeenCalledWith("attempt-2");
  expect(document.querySelector<HTMLElement>('[data-slot="enrolling-box"]')?.style.maxHeight).toBe("520px");
  expect(new Set(rows().map((row) => row.style.height))).toEqual(new Set(["52px"]));
});


it("counts Cloud slot occupancy while completed workflows still report joining", () => {
  const attempts = Array.from({ length: 9 }, (_, n) => attempt(n, { stage: "joining" }));
  render(<><EnrollmentCounters waiting={[]} attempts={attempts} joining={8} /><EnrollingList attempts={attempts} onCancel={vi.fn()} /></>);
  expect(screen.getAllByText("8 / 8")).toHaveLength(1);
  const stages = screen.getByRole("group", { name: "Enrollment stages" });
  expect(stages.querySelector('[data-counter="Joining"] dd')?.textContent).toBe("9");
  expect(screen.queryByText("9 / 8")).toBeNull();
});

it("keeps a fifty-host enrollment list within the ten-row rendering budget", () => {
  render(<EnrollingList attempts={Array.from({ length: 50 }, (_, n) => attempt(n + 1))} onCancel={vi.fn()} />);
  expect(screen.getByRole("table").getAttribute("aria-rowcount")).toBe("51");
  expect(rows().length).toBeLessThanOrEqual(14);
  expect(screen.getByText("0 of 50 joined")).toBeTruthy();
});

it("fills the expanded viewport when a second batch appends to a short list", () => {
  const onCancel = vi.fn();
  const view = render(<EnrollingList attempts={[attempt(1)]} onCancel={onCancel} />);
  const original = rows()[0];
  view.rerender(<EnrollingList attempts={Array.from({ length: 50 }, (_, n) => attempt(n + 1))} onCancel={onCancel} />);
  expect(rows()).toHaveLength(12);
  expect(rows()[0]).toBe(original);
  expect(screen.getByRole("button", { name: "Cancel web-10" })).toBeTruthy();
});

it("accounts for every open attempt exactly once, including publishing and an unknown stage", () => {
  const attempts = [attempt(1, { state: "instructed", queued: true }), attempt(2), attempt(3, { stage: "storage" }), attempt(4, { stage: "enrolling" }), attempt(5, { stage: "queued" }), attempt(6, { stage: "joining" }), attempt(7, { stage: "publishing" }), attempt(8, { stage: null })];
  render(<EnrollingList attempts={attempts} onCancel={vi.fn()} />);
  const values = [...screen.getByRole("group", { name: "Enrollment stages" }).querySelectorAll("dd")].map((node) => Number(node.textContent));
  expect(values).toEqual([2, 1, 1, 1, 1, 2]);
  expect(values.reduce((sum, n) => sum + n, 0)).toBe(attempts.length);
});

it("uses singular copy for exactly one server ahead", () => {
  render(<EnrollingList attempts={[attempt(1, { stage: "queued", ahead: 1 })]} onCancel={vi.fn()} />);
  expect(screen.getByText("1 server ahead")).toBeTruthy();
  expect(screen.queryByText("1 servers ahead")).toBeNull();
});
