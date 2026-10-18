// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { ServerEnrollmentView } from "#/modules/machines/waiting/enroll.server";
import { EnrollingList } from "./enrolling-list";

afterEach(cleanup);
const attempt = (n: number, patch: Partial<ServerEnrollmentView> = {}): ServerEnrollmentView => ({
  id: `attempt-${n}`, waitingServerId: `host-${n}`, role: "joiner", name: `web-${n}`, storage: { kind: "none" }, state: "running", stage: "installing", queued: false, ahead: null, machineId: null, failure: null, requestedAt: `2026-10-17T10:00:${String(n).padStart(2, "0")}Z`, endedAt: null, ...patch,
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
});

it("moves failed or cancelled hosts to the bottom of Waiting in return order", () => {
  const attempts = [attempt(2, { state: "failed", endedAt: "2026-10-17T10:02:00Z" }), attempt(1, { state: "cancelled", endedAt: "2026-10-17T10:01:00Z" })];
  render(<EnrollingList attempts={attempts} onCancel={vi.fn()} />);
  expect(screen.queryByRole("table")).toBeNull();
});


it("cancels the named attempt and holds every row at 52 pixels inside a ten-row box", () => {
  const onCancel = vi.fn();
  render(<EnrollingList attempts={Array.from({ length: 12 }, (_, n) => attempt(n + 1))} onCancel={onCancel} />);
  fireEvent.click(screen.getByRole("button", { name: "Cancel web-2" }));
  expect(onCancel).toHaveBeenCalledWith("attempt-2");
  expect(document.querySelector<HTMLElement>('[data-slot="enrolling-box"]')?.style.maxHeight).toBe("520px");
  expect(new Set(rows().map((row) => row.style.height))).toEqual(new Set(["52px"]));
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
  expect(rows().map((row) => within(row).getAllByRole("cell")[1]?.textContent)).toEqual(["Queued", "Installing", "Setting up ZFS", "Enrolling", "In line", "Joining", "Going live", "Starting"]);
});

it("uses singular copy for exactly one server ahead", () => {
  render(<EnrollingList attempts={[attempt(1, { stage: "queued", ahead: 1 })]} onCancel={vi.fn()} />);
  expect(screen.getByText("1 server ahead")).toBeTruthy();
  expect(screen.queryByText("1 servers ahead")).toBeNull();
});
