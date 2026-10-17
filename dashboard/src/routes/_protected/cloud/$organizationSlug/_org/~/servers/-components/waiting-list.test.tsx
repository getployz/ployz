// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Prep, StoredReport } from "#/modules/machines/waiting/checkin-wire";
import type { WaitingServerStatus, WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { SELECTION_MAX } from "#/modules/machines/waiting/probe";
import * as waitingFunctions from "#/modules/machines/waiting/waiting.functions";
import { PROBE_RETRY_MS, PROBE_SETTLE_MS } from "./use-waiting";
import { ROW_PX, VISIBLE_ROWS, WaitingList } from "./waiting-list";

let probe: ReturnType<typeof stubProbe>;
let reject: ReturnType<typeof stubReject>;
const stubProbe = () => vi.spyOn(waitingFunctions, "openProbeSelectionServerFn").mockResolvedValue({ probing: 1 });
const stubReject = () => vi.spyOn(waitingFunctions, "rejectWaitingServersServerFn").mockResolvedValue({ rejected: 0 });

beforeEach(() => {
  probe = stubProbe();
  reject = stubReject();
  vi.spyOn(waitingFunctions, "listWaitingServersServerFn").mockResolvedValue([]);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const NOW = Date.parse("2026-10-17T10:00:00Z");

const report: StoredReport = {
  hostname: "web",
  os: { id: "ubuntu", version: "24.04" },
  kernel: "6.8.0",
  arch: "x86_64",
  virt: "kvm",
  cpuCount: 4,
  memoryTotalBytes: 8 * 1024 ** 3,
  disks: [{ path: "/dev/sda", sizeBytes: 80 * 1024 ** 3, role: "root" }],
  docker: { kind: "absent" },
  cgroup: "v2",
  zfs: { kind: "eligible", targets: [{ kind: "main_disk" }] },
  blockers: [],
  warnings: [],
};

function host(hostname: string, options: { status?: WaitingServerStatus; prep?: Prep; blockers?: StoredReport["blockers"] } = {}): WaitingServerView {
  const base: StoredReport = { ...report, hostname, blockers: options.blockers ?? [] };
  const stored: StoredReport = options.prep === undefined ? base : { ...base, prep: options.prep };
  return {
    id: `id-${hostname}`,
    report: stored,
    observedIp: "203.0.113.7",
    fleetToken: "brisk-otter",
    status: options.status ?? "waiting",
    firstSeenAt: "2026-10-17T09:59:00Z",
    lastCheckInAt: "2026-10-17T09:59:55Z",
    probe: null,
  };
}

function renderList(servers: readonly WaitingServerView[], token?: string) {
  const queryClient = new QueryClient();
  const ui = (rows: readonly WaitingServerView[]) => (
    <QueryClientProvider client={queryClient}>
      <WaitingList organizationSlug="ada" servers={rows} now={NOW} token={token} />
    </QueryClientProvider>
  );
  const view = render(ui(servers));
  return { rerender: (rows: readonly WaitingServerView[]) => view.rerender(ui(rows)) };
}

const rowNames = () => within(box()).getAllByRole("row").map((row) => within(row).getByRole("button").textContent);
const box = () => document.querySelector<HTMLElement>('[data-slot="waiting-box"]') ?? expect.unreachable("no waiting box");
const pick = (hostname: string) => screen.getByRole("checkbox", { name: `Select ${hostname}` });
const statusOf = (hostname: string) => {
  const row = screen.getByRole("row", { name: new RegExp(hostname) });
  return within(row).getAllByRole("cell").at(-1)?.textContent;
};

it("appends a new arrival at the bottom and keeps every earlier row in place across ticks", () => {
  const first = [host("a"), host("b"), host("c")];
  const { rerender } = renderList(first);
  const before = within(box()).getAllByRole("row");

  rerender([...first, host("d")]);

  expect(rowNames()).toEqual(["a", "b", "c", "d"]);
  expect(within(box()).getAllByRole("row").slice(0, 3)).toEqual(before);
});

it("lets ready and preparing hosts be picked, and not blocked or offline ones", () => {
  renderList([
    host("ready"),
    host("preparing", { status: "preparing", prep: { docker: "installing", zfs: "installing" } }),
    host("blocked", { status: "blocked", blockers: [{ code: "disk_too_small", message: "Disk is 20 GB. Ployz needs 30 GB." }] }),
    host("offline", { status: "offline" }),
  ]);

  expect(pick("ready").hasAttribute("data-disabled")).toBe(false);
  expect(pick("preparing").hasAttribute("data-disabled")).toBe(false);
  expect(pick("blocked").hasAttribute("data-disabled")).toBe(true);
  expect(pick("offline").hasAttribute("data-disabled")).toBe(true);
});

it("reads each prep step in the status cell", () => {
  renderList([
    host("docker-installing", { status: "preparing", prep: { docker: "installing", zfs: "installing" } }),
    host("zfs-building", { prep: { docker: "ready", zfs: "installing" } }),
    host("all-ready", { prep: { docker: "ready", zfs: "ready" } }),
    host("docker-failed", {
      status: "preparing",
      prep: { docker: "failed", zfs: "off" },
      blockers: [{ code: "docker_install_failed", message: "apt-get exited 100. Run the line again." }],
    }),
    host("zfs-failed", { prep: { docker: "ready", zfs: "failed" } }),
  ]);

  expect(statusOf("docker-installing")).toBe("Installing Docker5s ago");
  expect(statusOf("zfs-building")).toBe("Building ZFS5s ago");
  expect(statusOf("all-ready")).toBe("Ready5s ago");
  expect(statusOf("docker-failed")).toBe("Docker install failedapt-get exited 100. Run the line again.");
  expect(statusOf("zfs-failed")).toMatch(/^ReadyZFS build failed/);
  expect(pick("zfs-failed").hasAttribute("data-disabled")).toBe(false);
  expect(pick("docker-failed").hasAttribute("data-disabled")).toBe(true);
});

it("holds the list in a box ten rows tall, every row the same height", () => {
  renderList(Array.from({ length: 12 }, (_, index) => host(`h${index}`)));

  expect(box().style.maxHeight).toBe(`${ROW_PX * VISIBLE_ROWS}px`);
  expect(box().style.maxHeight).toBe("520px");
  const heights = new Set(within(box()).getAllByRole("row").map((row) => row.style.height));
  expect([...heights]).toEqual([`${ROW_PX}px`]);
});

it("draws only the rows the box shows, not every waiting server", () => {
  renderList(Array.from({ length: 200 }, (_, index) => host(`h${index}`)));

  const drawn = rowNames();
  expect(drawn[0]).toBe("h0");
  expect(drawn.length).toBeGreaterThanOrEqual(VISIBLE_ROWS);
  expect(drawn.length).toBeLessThan(20);
  expect(screen.getAllByText("200").length).toBeGreaterThan(0);
});

const filterBy = (text: string) => fireEvent.change(screen.getByRole("textbox", { name: "Filter" }), { target: { value: text } });
const settle = (ms: number) => act(async () => { await vi.advanceTimersByTimeAsync(ms); });

it("rejects only the picked rows the filter still shows", async () => {
  renderList([host("web-1"), host("web-2"), host("db-1")]);
  fireEvent.click(pick("web-1"));
  fireEvent.click(pick("db-1"));

  filterBy("web");
  expect(screen.getByText("1 selected")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Reject" }));

  await vi.waitFor(() => expect(reject).toHaveBeenCalledOnce());
  expect(reject.mock.calls[0]?.[0]).toEqual({ data: { organizationSlug: "ada", ids: ["id-web-1"] } });
});

it("probes a selection once it stops changing, and asks again once a refused window has closed", async () => {
  vi.useFakeTimers();
  renderList([host("a"), host("b"), host("c")]);
  probe.mockResolvedValueOnce({ probing: 0 });

  fireEvent.click(pick("a"));
  await settle(PROBE_SETTLE_MS / 2);
  fireEvent.click(pick("b"));
  await settle(PROBE_SETTLE_MS / 2);
  fireEvent.click(pick("c"));
  await settle(PROBE_SETTLE_MS);

  expect(probe).toHaveBeenCalledOnce();
  expect(probe.mock.calls[0]?.[0]).toEqual({ data: { organizationSlug: "ada", ids: ["id-a", "id-b", "id-c"] } });

  await settle(PROBE_RETRY_MS);
  expect(probe).toHaveBeenCalledTimes(2);
  expect(probe.mock.calls[1]?.[0]).toEqual(probe.mock.calls[0]?.[0]);

  await settle(PROBE_RETRY_MS * 3);
  expect(probe).toHaveBeenCalledTimes(2);
});

it("shows every token when the one it was asked for has no waiting servers", () => {
  renderList([host("a"), host("b")], "no-such-token");

  expect(rowNames()).toEqual(["a", "b"]);
  expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Token" }).value).toBe("");
});

it("selects at most the cap, says so, and rejects no more than that", async () => {
  renderList(Array.from({ length: SELECTION_MAX + 5 }, (_, index) => host(`h${index}`)));

  fireEvent.click(screen.getByRole("checkbox", { name: "Select all" }));

  expect(screen.getByText(`${SELECTION_MAX} selected`)).toBeTruthy();
  expect(screen.getByText(`${SELECTION_MAX} max`)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Reject" }));
  await vi.waitFor(() => expect(reject).toHaveBeenCalledOnce());
  expect(reject.mock.calls[0]?.[0]?.data.ids).toHaveLength(SELECTION_MAX);
});
