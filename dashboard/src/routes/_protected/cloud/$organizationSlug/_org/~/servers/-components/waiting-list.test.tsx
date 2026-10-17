// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen, within } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { Prep, StoredReport } from "#/modules/machines/waiting/checkin-wire";
import type { WaitingServerStatus, WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { ROW_PX, VISIBLE_ROWS, WaitingList } from "./waiting-list";

afterEach(cleanup);

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

function renderList(servers: readonly WaitingServerView[]) {
  const queryClient = new QueryClient();
  const ui = (rows: readonly WaitingServerView[]) => (
    <QueryClientProvider client={queryClient}>
      <WaitingList organizationSlug="ada" servers={rows} now={NOW} />
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
