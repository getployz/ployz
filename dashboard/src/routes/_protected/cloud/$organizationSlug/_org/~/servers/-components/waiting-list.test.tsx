// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { Prep, StoredReport } from "#/modules/machines/waiting/checkin-wire";
import type { WaitingServerStatus, WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { SELECTION_MAX } from "#/modules/machines/waiting/probe";
import * as waitingFunctions from "#/modules/machines/waiting/waiting.functions";
import { ENROLL_REFUSED, PROBE_RETRY_MS, PROBE_SETTLE_MS } from "./use-waiting";
import type { ServerEnrollmentView } from "#/modules/machines/waiting/enroll.server";
import { serverEnrollmentsQueryOptions, waitingServersQueryOptions } from "#/modules/machines/waiting/waiting.queries";
import { clientWithRole, expectGate, ROLE_STATES, seedRole } from "#/test/your-role";
import { EnrollSheet } from "./enroll-sheet";
import { IntakeTray, ROW_PX, VISIBLE_ROWS, WaitingList } from "./waiting-list";

let probe: ReturnType<typeof stubProbe>;
let reject: ReturnType<typeof stubReject>;
const stubProbe = () => vi.spyOn(waitingFunctions, "openProbeSelectionServerFn").mockResolvedValue({ probing: 1 });
const stubReject = () => vi.spyOn(waitingFunctions, "rejectWaitingServersServerFn").mockResolvedValue({ rejected: 0 });

beforeEach(() => {
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: false, media: query, addEventListener() {}, removeEventListener() {} }));
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
    arrivalAt: "2026-10-17T09:59:00Z",
    returned: null,
  };
}

function renderList(servers: readonly WaitingServerView[], token?: string) {
  const queryClient = seedRole(new QueryClient(), "ada", "owner");
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
  expect(statusOf("zfs-building")).toBe("ReadyBuilding ZFS · 5s ago");
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
  renderList([host("web-1"), host("web-2"), host("db-1"), ...Array.from({ length: VISIBLE_ROWS }, (_, index) => host(`db-${index + 2}`))]);
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

it("retries only the current selection when an older probe answers after the selection moved on", async () => {
  vi.useFakeTimers();
  renderList([host("a"), host("b"), host("c")]);
  // Cloud refuses any selection holding a host whose window is still open.
  const openUntil = new Map<string, number>();
  let answerStale: (value: { probing: number }) => void = () => {};
  probe.mockImplementation(({ data }) => {
    if (data.ids.length === 2) return new Promise((resolve) => { answerStale = resolve; });
    if (data.ids.some((id) => (openUntil.get(id) ?? 0) > Date.now())) return Promise.resolve({ probing: 0 });
    for (const id of data.ids) openUntil.set(id, Date.now() + PROBE_RETRY_MS);
    return Promise.resolve({ probing: data.ids.length });
  });

  fireEvent.click(pick("a"));
  await settle(PROBE_SETTLE_MS);
  fireEvent.click(pick("b"));
  await settle(PROBE_SETTLE_MS);
  fireEvent.click(pick("c"));
  await act(async () => answerStale({ probing: 0 }));
  await settle(PROBE_SETTLE_MS);
  await settle(PROBE_RETRY_MS * 4);

  expect(probe.mock.calls.map(([call]) => call?.data.ids)).toEqual([
    ["id-a"],
    ["id-a", "id-b"],
    ["id-a", "id-b", "id-c"],
    ["id-a", "id-b", "id-c"],
  ]);
  await expect(probe.mock.results.at(-1)?.value).resolves.toEqual({ probing: 3 });
});

it("asks nothing more once the list is gone, even when a pending probe comes back refused", async () => {
  vi.useFakeTimers();
  let answer: (value: { probing: number }) => void = () => {};
  probe.mockImplementation(() => new Promise((resolve) => { answer = resolve; }));
  renderList([host("a")]);

  fireEvent.click(pick("a"));
  await settle(PROBE_SETTLE_MS);
  expect(probe).toHaveBeenCalledOnce();
  cleanup();
  await act(async () => answer({ probing: 0 }));
  await settle(PROBE_RETRY_MS * 3);

  expect(probe).toHaveBeenCalledOnce();
});

it("shows no filters while every waiting server fits in the box", () => {
  renderList([host("a"), host("b")]);

  expect(rowNames()).toEqual(["a", "b"]);
  expect(screen.queryByRole("textbox", { name: "Filter" })).toBeNull();
});

it("a token link filters even a short list", () => {
  renderList([host("a"), { ...host("b"), fleetToken: "calm-heron" }], "calm-heron");

  expect(rowNames()).toEqual(["b"]);
  expect(screen.getByRole<HTMLSelectElement>("combobox", { name: "Token" }).value).toBe("calm-heron");
});

it("shows every token when the one it was asked for has no waiting servers", () => {
  const names = Array.from({ length: VISIBLE_ROWS + 1 }, (_, index) => `h${String(index).padStart(2, "0")}`);
  renderList(names.map((name) => host(name)), "no-such-token");

  expect(rowNames()).toEqual(names.slice(0, rowNames().length));
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

const planFor = (ids: readonly string[]) => ({
  rows: ids.map((id) => ({ waitingServerId: id, fleetTokenId: "fleet", hostname: id.replace("id-", ""), role: "joiner" as const,
    instruction: { name: id.replace("id-", ""), storage: { kind: "zfs" as const }, policy: { labels: {}, acceptsBuilds: true, acceptsServices: true, acceptsIngress: true } } })),
  skipped: [], spread: { providers: 1, networks: 1 },
});
function deferred<T>() {
  let resolve: (value: T) => void = () => {};
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}
// React Query tells its observers on a setTimeout(0), which act does not wait for.
const notified = () => new Promise((done) => setTimeout(done, 0));
const flowClient = () => seedRole(new QueryClient({ defaultOptions: { queries: { retry: false } } }), "ada", "owner");
function flow(servers: readonly WaitingServerView[], hasFirst: boolean) {
  const client = flowClient();
  const ui = (rows: readonly WaitingServerView[], founded: boolean) => <QueryClientProvider client={client}><WaitingList organizationSlug="ada" servers={rows} now={NOW} hasFirst={founded} /></QueryClientProvider>;
  const view = render(ui(servers, hasFirst));
  return { client, rerender: (rows = servers, founded = hasFirst) => view.rerender(ui(rows, founded)) };
}
function previewStub() {
  return vi.spyOn(waitingFunctions, "planServerEnrollmentServerFn").mockImplementation(async ({ data }) => planFor(data.ids));
}

it.each(["waiting-first", "status-first"])("keeps returned rows at their effective arrival for deferred %s responses", async (order) => {
  const first = { ...host("first"), firstSeenAt: "2026-10-17T08:00:00Z", arrivalAt: "2026-10-17T08:00:00Z" };
  const second = host("second");
  const attempt: ServerEnrollmentView = { id: "attempt", waitingServerId: first.id, role: "joiner", name: "first", storage: { kind: "none" }, state: "running", stage: "installing", queued: false, ahead: null, machineId: null, failure: null, requestedAt: "2026-10-17T09:00:00Z", endedAt: null };
  const client = flowClient();
  const waitingKey = waitingServersQueryOptions("ada").queryKey;
  const statusKey = serverEnrollmentsQueryOptions("ada").queryKey;
  client.setQueryData(waitingKey, [second]);
  client.setQueryData(statusKey, { attempts: [attempt], hasFirst: true, joining: 0 });
  render(<QueryClientProvider client={client}><IntakeTray organizationSlug="ada" token={undefined} firstRun={false} /></QueryClientProvider>);
  expect(rowNames()).toEqual(["second"]);
  const waiting = deferred<WaitingServerView[]>();
  const status = deferred<Awaited<ReturnType<typeof waitingFunctions.serverEnrollmentStatusServerFn>>>();
  vi.spyOn(waitingFunctions, "listWaitingServersServerFn").mockReturnValue(waiting.promise);
  vi.spyOn(waitingFunctions, "serverEnrollmentStatusServerFn").mockReturnValue(status.promise);
  const waitingFetch = client.refetchQueries({ queryKey: waitingKey });
  const statusFetch = client.refetchQueries({ queryKey: statusKey });
  const returned: WaitingServerView = { ...first, arrivalAt: "2026-10-17T10:01:00Z", returned: { state: "failed", stage: "storage", message: "Disk missing" } };
  const endWaiting = () => act(async () => { waiting.resolve([second, returned]); await waitingFetch; await notified(); });
  const endStatus = () => act(async () => { status.resolve({ attempts: [{ ...attempt, state: "failed", endedAt: returned.arrivalAt }], hasFirst: true, joining: 0 }); await statusFetch; await notified(); });
  if (order === "waiting-first") {
    await endWaiting();
    expect(rowNames()).toEqual(["second", "first"]);
    const before = within(box()).getAllByRole("row");
    await endStatus();
    expect(within(box()).getAllByRole("row")).toEqual(before);
  } else {
    await endStatus();
    expect(rowNames()).toEqual(["second"]);
    await endWaiting();
  }
  expect(rowNames()).toEqual(["second", "first"]);
});

it.each(["failed", "cancelled"] as const)("keeps the live status primary after an attempt is %s", (state) => {
  const row: WaitingServerView = { ...host("returned", { status: "offline" }), returned: { state, stage: "storage", message: "Disk missing" } };
  const { rerender } = renderList([row]);
  expect(statusOf("returned")).toMatch(/^Offline/);
  expect(statusOf("returned")).toContain(`Last enroll ${state}`);
  expect(statusOf("returned")).not.toContain("Setting up ZFS");
  expect(statusOf("returned")).not.toContain("Disk missing");
  rerender([{ ...row, status: "blocked", report: { ...row.report, blockers: [{ code: "docker_stopped", message: "Start Docker" }] } }]);
  expect(statusOf("returned")).toMatch(/^Docker stoppedStart Docker/);
  rerender([{ ...row, status: "waiting" }]);
  expect(statusOf("returned")).toMatch(/^Ready/);
});

it("drives selection, first-server preview and the exact start payload, then clears the picks", async () => {
  const preview = previewStub();
  const start = vi.spyOn(waitingFunctions, "startServerEnrollmentServerFn").mockResolvedValue({ attempts: [], skipped: [], spread: { providers: 1, networks: 1 } });
  flow([host("one"), host("two")], false);
  fireEvent.click(pick("one"));
  fireEvent.click(pick("two"));
  fireEvent.click(screen.getByRole("button", { name: "Enroll 2" }));
  await vi.waitFor(() => expect(screen.getByRole("region", { name: "Enrollment preview" })).toBeTruthy());
  expect(preview.mock.calls.at(-1)?.[0]?.data).toMatchObject({ ids: ["id-one", "id-two"], first: "id-one", storage: { kind: "zfs", useSpareDisk: false } });
  fireEvent.change(screen.getByLabelText("First server"), { target: { value: "id-two" } });
  await vi.waitFor(() => expect(preview.mock.calls.at(-1)?.[0]?.data.first).toBe("id-two"));
  await vi.waitFor(() => expect(screen.getAllByRole<HTMLButtonElement>("button", { name: "Enroll 2" }).at(-1)?.disabled).toBe(false));
  fireEvent.click(screen.getAllByRole("button", { name: "Enroll 2" }).at(-1) ?? expect.unreachable());
  await vi.waitFor(() => expect(start).toHaveBeenCalledOnce());
  expect(start.mock.calls[0]?.[0]?.data).toEqual({ organizationSlug: "ada", ids: ["id-one", "id-two"], first: "id-two", storage: { kind: "zfs", useSpareDisk: false, fallbackToDocker: true }, names: { kind: "hostnames" }, runs: { services: true, ingress: true, builds: true } });
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(screen.queryByText("2 selected")).toBeNull();
});

it.each([{ count: 1, hasFirst: false, status: "waiting" }, { count: 1, hasFirst: true, status: "waiting" }, { count: 2, hasFirst: false, status: "waiting" }, { count: 1, hasFirst: false, status: "offline" }] as const)("keeps the sheet closed for $count hosts, founder=$hasFirst, status=$status", ({ count, hasFirst, status }) => {
  previewStub();
  flow(Array.from({ length: count }, (_, n) => host(String(n), { status })), hasFirst);
  expect(screen.queryByRole("dialog")).toBeNull();
});

it("omits first for an existing cluster and clears selection when the sheet is closed", async () => {
  const preview = previewStub();
  const start = vi.spyOn(waitingFunctions, "startServerEnrollmentServerFn").mockResolvedValue({ attempts: [], skipped: [], spread: { providers: 1, networks: 1 } });
  flow([host("one")], true);
  fireEvent.click(pick("one"));
  fireEvent.click(screen.getByRole("button", { name: "Enroll 1" }));
  await vi.waitFor(() => expect(screen.getAllByRole<HTMLButtonElement>("button", { name: "Enroll 1" }).at(-1)?.disabled).toBe(false));
  expect(preview.mock.calls.at(-1)?.[0]?.data).not.toHaveProperty("first");
  fireEvent.click(screen.getAllByRole("button", { name: "Enroll 1" }).at(-1) ?? expect.unreachable());
  await vi.waitFor(() => expect(start).toHaveBeenCalledOnce());
  expect(start.mock.calls[0]?.[0]?.data).not.toHaveProperty("first");
  await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  fireEvent.click(pick("one"));
  fireEvent.click(screen.getByRole("button", { name: "Enroll 1" }));
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(screen.queryByText("1 selected")).toBeNull();
});

it("does not offer an unproven spare-disk promise, and cannot enroll a zero-row plan", async () => {
  vi.spyOn(waitingFunctions, "planServerEnrollmentServerFn").mockResolvedValue(planFor([]));
  const row = host("spare");
  const withDisk: WaitingServerView = { ...row, report: { ...row.report, disks: [{ role: "empty", sizeBytes: 100 * 1024 ** 3 }] } };
  flow([withDisk], false);
  fireEvent.click(pick("spare"));
  fireEvent.click(screen.getByRole("button", { name: "Enroll 1" }));
  await vi.waitFor(() => expect(screen.getByRole("button", { name: "Enroll 0" })).toBeTruthy());
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Enroll 0" }).disabled).toBe(true);
  expect(screen.queryByLabelText("Use a spare disk if there is one")).toBeNull();
  expect(screen.queryByText("Main or empty disk")).toBeNull();
  expect(screen.getByText("ZFS uses the main disk.")).toBeTruthy();
});

it("keeps preview data while editing, debounces names, and permits a background refetch", async () => {
  const preview = previewStub();
  const client = flowClient();
  render(<QueryClientProvider client={client}><EnrollSheet organizationSlug="ada" servers={[host("one")]} hasFirst onClose={vi.fn()} /></QueryClientProvider>);
  await vi.waitFor(() => expect(screen.getByText("ZFS on main disk")).toBeTruthy());
  const pendingPlan = deferred<ReturnType<typeof planFor>>();
  preview.mockReturnValueOnce(pendingPlan.promise);
  const refetch = client.invalidateQueries({ queryKey: ["server-enrollment-plan"] });
  await vi.waitFor(() => expect(preview).toHaveBeenCalledTimes(2));
  await act(async () => { await new Promise((resolve) => setTimeout(resolve, 20)); });
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Enroll 1" }).disabled).toBe(false);
  await act(async () => { pendingPlan.resolve(planFor(["id-one"])); await refetch; });
  fireEvent.change(screen.getByRole("combobox", { name: "Names" }), { target: { value: "pattern" } });
  await vi.waitFor(() => expect(preview).toHaveBeenCalledTimes(3));
  await vi.waitFor(() => expect(screen.getByRole<HTMLButtonElement>("button", { name: "Enroll 1" }).disabled).toBe(false));
  const baseline = preview.mock.calls.length;
  fireEvent.change(screen.getByLabelText("Name pattern"), { target: { value: "api-{n}" } });
  expect(preview).toHaveBeenCalledTimes(baseline);
  expect(screen.getByText("ZFS on main disk")).toBeTruthy();
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Enroll 1" }).disabled).toBe(true);
  const edited = deferred<ReturnType<typeof planFor>>();
  preview.mockReturnValueOnce(edited.promise);
  await vi.waitFor(() => expect(preview.mock.calls.at(-1)?.[0]?.data.names).toEqual({ kind: "pattern", pattern: "api-{n}" }));
  expect(screen.getByText("ZFS on main disk")).toBeTruthy();
  await act(async () => edited.resolve(planFor(["id-one"])));
  await vi.waitFor(() => expect(screen.getByRole<HTMLButtonElement>("button", { name: "Enroll 1" }).disabled).toBe(false));
});

it("heads the tray with the readiness mix, hiding empty parts, and shows CPU, RAM and disk per row", () => {
  renderList([host("alpha"), host("bravo"), host("charlie", { status: "blocked", blockers: [{ code: "no_systemd", message: "Needs systemd" }] })]);
  expect(screen.getByRole("heading", { name: "Waiting to join" })).toBeTruthy();
  expect(screen.getByText("2 ready")).toBeTruthy();
  expect(screen.getByText("1 blocked")).toBeTruthy();
  expect(screen.queryByText("0 offline")).toBeNull();
  expect([...document.querySelectorAll<HTMLElement>("[data-mix]")].map((part) => [part.dataset["mix"], part.style.flex])).toEqual([["ready", "2 1 0%"], ["blocked", "1 1 0%"]]);
  expect(screen.getAllByText("4 vCPU · 8 GB · 80 GB")).toHaveLength(3);
  expect(statusOf("charlie")).toBe("No systemdNeeds systemd");
});

it("names the first arrival on first run and enrolls it from the heading", async () => {
  vi.spyOn(waitingFunctions, "planServerEnrollmentServerFn").mockImplementation(async ({ data }) => planFor(data.ids));
  render(<QueryClientProvider client={flowClient()}><WaitingList organizationSlug="ada" servers={[host("web")]} now={NOW} firstRun /></QueryClientProvider>);
  expect(screen.getByRole("heading", { name: "web is ready to enroll" })).toBeTruthy();
  expect(screen.queryByRole("heading", { name: "Waiting to join" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Enroll" }));
  await vi.waitFor(() => expect(screen.getByRole("region", { name: "Enrollment preview" })).toBeTruthy());
});

it("enrolls the server its detail sheet shows", async () => {
  const preview = previewStub();
  const one = host("one");
  vi.spyOn(waitingFunctions, "showWaitingServerServerFn").mockResolvedValue({ ...one, report, probe: null } as never);
  flow([one, host("two")], true);
  fireEvent.click(screen.getByRole("button", { name: "one" }));
  const sheet = await screen.findByRole("dialog");
  fireEvent.click(await within(sheet).findByRole("button", { name: "Enroll" }));
  await vi.waitFor(() => expect(preview.mock.calls.at(-1)?.[0]?.data.ids).toEqual([one.id]));
});

async function listAs(state: (typeof ROLE_STATES)[number], servers: readonly WaitingServerView[], firstRun = false) {
  const client = await clientWithRole("ada", state);
  render(<QueryClientProvider client={client}><WaitingList organizationSlug="ada" servers={servers} now={NOW} firstRun={firstRun} /></QueryClientProvider>);
}

it.each(ROLE_STATES)("lets only a role that may add servers enroll or reject the first arrival (%s)", async (state) => {
  await listAs(state, [host("web")], true);
  expectGate(screen.getByRole("button", { name: "Enroll" }), state, "servers:add", ENROLL_REFUSED);
  expectGate(screen.getByRole("button", { name: "Reject" }), state, "servers:add", ENROLL_REFUSED);
});

it.each(ROLE_STATES)("lets only a role that may add servers enroll or reject the picked servers (%s)", async (state) => {
  await listAs(state, [host("one"), host("two")]);
  fireEvent.click(pick("one"));
  expectGate(screen.getByRole("button", { name: "Enroll 1" }), state, "servers:add", ENROLL_REFUSED);
  expectGate(screen.getByRole("button", { name: "Reject" }), state, "servers:add", ENROLL_REFUSED);
});

it.each(ROLE_STATES)("lets only a role that may add servers enroll or reject from the detail sheet (%s)", async (state) => {
  const one = host("one");
  vi.spyOn(waitingFunctions, "showWaitingServerServerFn").mockResolvedValue({ ...one, report, probe: null } as never);
  await listAs(state, [one, host("two")]);
  fireEvent.click(screen.getByRole("button", { name: "one" }));
  const sheet = await screen.findByRole("dialog");
  expectGate(await within(sheet).findByRole("button", { name: "Enroll" }), state, "servers:add", ENROLL_REFUSED);
  expectGate(within(sheet).getByRole("button", { name: "Reject" }), state, "servers:add", ENROLL_REFUSED);
});

it.each([["failed", "Failed at Setting up ZFS"], ["cancelled", "Cancelled"]] as const)("shows a %s attempt's stage and message in the detail sheet", async (state, reason) => {
  const one: WaitingServerView = { ...host("one"), returned: { state, stage: "storage", message: "Disk missing" } };
  vi.spyOn(waitingFunctions, "showWaitingServerServerFn").mockResolvedValue({ ...one, report, probe: null } as never);
  renderList([one]);
  fireEvent.click(screen.getByRole("button", { name: "one" }));
  const last = await within(await screen.findByRole("dialog")).findByRole("region", { name: "Last enroll" });
  expect(last.textContent).toContain(reason);
  expect(last.textContent).toContain("Disk missing");
});

it("says nothing joins on its own until the first server is enrolled by hand", () => {
  const hint = "No cluster yet. Enroll one server to start it.";
  const client = new QueryClient();
  const { rerender } = render(<QueryClientProvider client={client}><WaitingList organizationSlug="ada" servers={[host("one"), host("two")]} now={NOW} hasFirst={false} /></QueryClientProvider>);
  expect(screen.getByText(hint)).toBeTruthy();
  rerender(<QueryClientProvider client={client}><WaitingList organizationSlug="ada" servers={[host("one"), host("two")]} now={NOW} hasFirst /></QueryClientProvider>);
  expect(screen.queryByText(hint)).toBeNull();
});
